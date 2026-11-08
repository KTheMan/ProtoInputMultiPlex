//! Ephemeral, append-only generated seats owned by one supervised routing session.
use crate::{macro_studio, routing_engine::Engine};
use anyhow::{Context, Result, ensure};
use mux_core::{DeviceKind, Profile, ResolvedSeat, Seat, emulated, replication::EdgeOptions};
use rmcp::schemars;
use serde::{Deserialize, Serialize};

#[derive(Clone, Serialize, Deserialize, rmcp::schemars::JsonSchema)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum Command {
    Inspect,
    Start {
        source_id: String,
        target_pid: u32,
        name: String,
        #[serde(default)]
        options: EdgeOptions,
        expected_revision: Option<u64>,
    },
    Stop {
        id: String,
        expected_revision: Option<u64>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Slot {
    pub user: usize,
    pub seat: Seat,
    pub active: bool,
    pub clone_of: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct State {
    pub acknowledged: bool,
    pub revision: u64,
    pub slots: Vec<Slot>,
    pub error: Option<String>,
}

pub struct Runtime {
    resolved: Vec<ResolvedSeat>,
    state: State,
    desktop: Option<mux_core::routing::DesktopRouting>,
    controller_pid: u32,
    next_device: u32,
}

impl Runtime {
    pub fn new(
        profile: &Profile,
        resolved: Vec<ResolvedSeat>,
        controller_pid: u32,
    ) -> Result<Self> {
        let mut next_device = 1;
        for seat in &profile.seats {
            for path in std::iter::once(&seat.mouse).chain(seat.keyboard.iter()) {
                if let Some(device) = emulated::parse(path)? {
                    let id = (device.handle as u32 & !emulated::MASK) >> 1;
                    next_device = next_device.max(id + 1);
                }
            }
        }
        Ok(Self {
            resolved,
            state: State {
                slots: profile
                    .seats
                    .iter()
                    .cloned()
                    .enumerate()
                    .map(|(user, seat)| Slot {
                        user,
                        seat,
                        active: true,
                        clone_of: None,
                    })
                    .collect(),
                ..Default::default()
            },
            desktop: profile.desktop.clone(),
            controller_pid,
            next_device,
        })
    }

    pub fn state(&self) -> State {
        self.state.clone()
    }
    pub fn require_active(&self, user: usize) -> Result<&Slot> {
        let slot = self.state.slots.get(user).context("Unknown user")?;
        ensure!(slot.active, "This generated user has been removed");
        Ok(slot)
    }
    pub fn require_device(&self, device: u32) -> Result<()> {
        ensure!(
            self.state
                .slots
                .iter()
                .zip(&self.resolved)
                .any(|(slot, seat)| slot.active
                    && (seat.mouse == device || seat.keyboard == Some(device))),
            "Unknown or removed input device"
        );
        Ok(())
    }
    pub fn resolved(&self, user: usize) -> Result<&ResolvedSeat> {
        self.require_active(user)?;
        Ok(&self.resolved[user])
    }
    pub fn settings(&mut self, user: usize, settings: mux_core::desktop::CursorSettings) {
        self.state.slots[user].seat.cursor = settings;
    }
    pub fn output_settings(&mut self, user: usize, settings: mux_core::output::Settings) {
        self.state.slots[user].seat.output = settings;
    }
    pub fn magnifier_settings(&mut self, user: usize, settings: mux_core::magnifier::Settings) {
        self.state.slots[user].seat.magnifier = settings;
    }
    fn revision(&self, expected: Option<u64>) -> Result<u64> {
        ensure!(
            expected.is_none_or(|r| r == self.state.revision),
            "Generated user roster changed; inspect its current revision"
        );
        self.state
            .revision
            .checked_add(1)
            .context("Generated user revision exhausted")
    }
    pub fn command(
        &mut self,
        command: Command,
        automation: &mut macro_studio::Runtime,
        router: &mut dyn Engine,
    ) -> Result<State> {
        match command {
            Command::Inspect => {}
            Command::Start {
                source_id,
                target_pid,
                name,
                options,
                expected_revision,
            } => {
                let revision = self.revision(expected_revision)?;
                let desktop = self
                    .desktop
                    .as_ref()
                    .context("Live generated users require automatic desktop routing")?;
                ensure!(
                    target_pid != 0
                        && target_pid != std::process::id()
                        && target_pid != self.controller_pid,
                    "Select an eligible target process"
                );
                ensure!(
                    desktop.allowed_pids.is_empty() || desktop.allowed_pids.contains(&target_pid),
                    "Clone target must already be allowed by this session"
                );
                let source = self
                    .state
                    .slots
                    .iter()
                    .position(|s| s.active && s.seat.identity() == source_id)
                    .context("Unknown or removed clone source")?;
                router.pointer(source)?;
                ensure!(
                    !name.trim().is_empty() && name.chars().count() <= 64,
                    "Provide a generated user name of 1 to 64 characters"
                );
                ensure!(
                    !self.state.slots.iter().any(|s| s.seat.name == name),
                    "Generated user names must remain distinct within this session"
                );
                let mouse = emulated::device(DeviceKind::Mouse, self.next_device)?;
                let keyboard = emulated::device(DeviceKind::Keyboard, self.next_device)?;
                // IDs are never recycled, including after a failed native registration attempt.
                self.next_device += 1;
                let seat = Seat {
                    id: Seat::new_id(),
                    name: name.clone(),
                    mouse: mouse.path,
                    keyboard: Some(keyboard.path),
                    target_pid,
                    desktop_start: None,
                    cursor: self.state.slots[source].seat.cursor.clone(),
                    annotation: Default::default(),
                    magnifier: Default::default(),
                    output: self.state.slots[source].seat.output.clone(),
                };
                mux_windows::monitors::validate_settings(&seat.cursor)?;
                let resolved = ResolvedSeat {
                    name,
                    mouse: mouse.handle as u32,
                    keyboard: Some(keyboard.handle as u32),
                    target_pid,
                };
                let prepared = automation.prepare_virtual_user(
                    resolved.clone(),
                    seat.identity(),
                    source,
                    options,
                )?;
                self.state.slots.try_reserve(1)?;
                self.resolved.try_reserve(1)?;
                let user = router.add_virtual_user(&seat, resolved.clone())?;
                ensure!(
                    user == self.state.slots.len(),
                    "Routing user slots must be append-only"
                );
                automation.add_virtual_user(resolved.clone(), prepared, seat.output.clone());
                self.resolved.push(resolved);
                self.state.slots.push(Slot {
                    user,
                    seat,
                    active: true,
                    clone_of: Some(source_id),
                });
                self.state.revision = revision;
            }
            Command::Stop {
                id,
                expected_revision,
            } => {
                let revision = self.revision(expected_revision)?;
                let user = self
                    .state
                    .slots
                    .iter()
                    .position(|s| s.active && s.seat.identity() == id)
                    .context("Unknown or removed generated user")?;
                ensure!(
                    self.state.slots[user].clone_of.is_some(),
                    "Original session users cannot be removed through clone lifecycle"
                );
                automation.remove_virtual_user(user, router)?;
                // Keep the reserved slot and identity: delayed commands can never retarget it.
                self.state.slots[user].active = false;
                self.state.revision = revision;
            }
        }
        let mut state = self.state();
        state.acknowledged = true;
        Ok(state)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{router::State as RoutingState, routing_engine::ProgramOrigin};
    use mux_core::{
        desktop::CursorSettings,
        emulated::Packet,
        routing::{DesktopRouting, Mode, Window},
    };
    use std::collections::BTreeSet;

    #[derive(Default)]
    struct FakeEngine {
        active: Vec<bool>,
        devices: Vec<(u32, Option<u32>)>,
        pointers: Vec<(i32, i32)>,
        holds: BTreeSet<(u32, u32)>,
        add_calls: usize,
        remove_calls: Vec<usize>,
        failing_release: Option<(u32, u32)>,
        release_all_calls: usize,
        input_calls: usize,
        released_devices: Vec<u32>,
    }

    impl FakeEngine {
        fn new(seats: &[ResolvedSeat]) -> Self {
            Self {
                active: vec![true; seats.len()],
                devices: seats
                    .iter()
                    .map(|seat| (seat.mouse, seat.keyboard))
                    .collect(),
                pointers: vec![(40, 60); seats.len()],
                ..Default::default()
            }
        }
    }

    impl Engine for FakeEngine {
        fn add_virtual_user(&mut self, _: &Seat, seat: ResolvedSeat) -> Result<usize> {
            let user = self.active.len();
            self.active.push(true);
            self.devices.push((seat.mouse, seat.keyboard));
            self.pointers.push((40, 60));
            self.add_calls += 1;
            Ok(user)
        }
        fn prepare_remove_virtual_user(&mut self, user: usize) -> Result<()> {
            ensure!(self.active.get(user) == Some(&true), "unknown fake user");
            let (mouse, keyboard) = self.devices[user];
            self.holds.retain(|&(device, key)| {
                if self.failing_release == Some((device, key)) {
                    true
                } else if device == mouse || Some(device) == keyboard {
                    self.released_devices.push(device);
                    false
                } else {
                    true
                }
            });
            ensure!(
                !self.failing_release.is_some_and(|(device, key)| {
                    (device == mouse || Some(device) == keyboard)
                        && self.holds.contains(&(device, key))
                }),
                "injected native release failure"
            );
            Ok(())
        }
        fn commit_remove_virtual_user(&mut self, user: usize) {
            self.active[user] = false;
            self.remove_calls.push(user);
        }
        fn program_origin(&self) -> ProgramOrigin {
            ProgramOrigin::Composite
        }
        fn pointer(&self, user: usize) -> Result<(i32, i32)> {
            ensure!(self.active.get(user) == Some(&true), "retired fake user");
            Ok(self.pointers[user])
        }
        fn input(&mut self, packet: Packet) -> Result<()> {
            self.input_calls += 1;
            match packet.kind {
                1 if packet.flags & 1 == 0 => {
                    self.holds.insert((packet.device, packet.data));
                }
                1 => {
                    self.holds.remove(&(packet.device, packet.data));
                    self.released_devices.push(packet.device);
                }
                _ => {}
            }
            Ok(())
        }
        fn key_held(&self, device: u32, key: u32) -> bool {
            self.holds.contains(&(device, key))
        }
        fn button_held(&self, _: u32, _: u32) -> bool {
            false
        }
        fn paused(&self) -> bool {
            false
        }
        fn release_all(&mut self) -> Result<()> {
            self.release_all_calls += 1;
            self.holds.clear();
            Ok(())
        }
        fn pause(&mut self, _: bool) -> Result<()> {
            Ok(())
        }
        fn settings(&mut self, _: usize, _: CursorSettings) -> Result<()> {
            Ok(())
        }
        fn tick(&mut self) -> Result<(bool, Vec<RoutingState>)> {
            Ok((false, Vec::new()))
        }
        fn selected_window(&self, _: usize) -> Result<Option<Window>> {
            Ok(None)
        }
        fn move_to(&mut self, user: usize, x: i32, y: i32) -> Result<()> {
            ensure!(self.active.get(user) == Some(&true), "retired fake user");
            self.pointers[user] = (x, y);
            Ok(())
        }
    }

    fn seat(id: &str, name: &str, device_id: u32, target_pid: u32) -> Seat {
        Seat {
            id: id.into(),
            name: name.into(),
            mouse: emulated::device(DeviceKind::Mouse, device_id).unwrap().path,
            keyboard: Some(
                emulated::device(DeviceKind::Keyboard, device_id)
                    .unwrap()
                    .path,
            ),
            target_pid,
            desktop_start: None,
            cursor: CursorSettings::default(),
            annotation: Default::default(),
            magnifier: Default::default(),
            output: Default::default(),
        }
    }

    fn resolved(seat: &Seat) -> ResolvedSeat {
        let mouse = emulated::parse(&seat.mouse).unwrap().unwrap();
        let keyboard = emulated::parse(seat.keyboard.as_deref().unwrap())
            .unwrap()
            .unwrap();
        ResolvedSeat {
            name: seat.name.clone(),
            mouse: mouse.handle as u32,
            keyboard: Some(keyboard.handle as u32),
            target_pid: seat.target_pid,
        }
    }

    fn runtimes() -> (Runtime, macro_studio::Runtime, FakeEngine, Seat) {
        let original = seat("seat-original", "Original", 1, 101);
        let profile = Profile {
            version: 1,
            concurrent: false,
            desktop: Some(DesktopRouting {
                mode: Mode::Concurrent,
                multi_keyboard: true,
                allowed_pids: vec![202, 303],
            }),
            replication: Default::default(),
            forwarding: Default::default(),
            mapper: Default::default(),
            seats: vec![original.clone()],
        };
        let seats = vec![resolved(&original)];
        let runtime = Runtime::new(&profile, seats.clone(), 909).unwrap();
        let studio = macro_studio::Runtime::new_with_ids(seats.clone(), vec![original.identity()]);
        (runtime, studio, FakeEngine::new(&seats), original)
    }

    struct StartSpec<'a> {
        source_id: &'a str,
        target_pid: u32,
        name: &'a str,
        expected_revision: Option<u64>,
        options: EdgeOptions,
    }

    fn start(
        runtime: &mut Runtime,
        studio: &mut macro_studio::Runtime,
        engine: &mut FakeEngine,
        spec: StartSpec<'_>,
    ) -> Result<State> {
        runtime.command(
            Command::Start {
                source_id: spec.source_id.into(),
                target_pid: spec.target_pid,
                name: spec.name.into(),
                options: spec.options,
                expected_revision: spec.expected_revision,
            },
            studio,
            engine,
        )
    }

    fn roster(runtime: &Runtime) -> Vec<(usize, String, String, Option<String>, bool)> {
        runtime
            .state()
            .slots
            .into_iter()
            .map(|slot| {
                (
                    slot.user,
                    slot.seat.identity(),
                    slot.seat.mouse,
                    slot.seat.keyboard,
                    slot.active,
                )
            })
            .collect()
    }

    #[test]
    fn rejected_starts_and_original_stop_leave_roster_and_graph_unchanged() {
        let (mut runtime, mut studio, mut engine, original) = runtimes();
        let before_roster = roster(&runtime);
        let before_edges = studio.state(false).edges;

        assert!(
            start(
                &mut runtime,
                &mut studio,
                &mut engine,
                StartSpec {
                    source_id: "unknown-source",
                    target_pid: 202,
                    name: "Bad source",
                    expected_revision: Some(0),
                    options: EdgeOptions::default(),
                },
            )
            .is_err()
        );
        assert!(
            start(
                &mut runtime,
                &mut studio,
                &mut engine,
                StartSpec {
                    source_id: &original.id,
                    target_pid: 404,
                    name: "Out of scope",
                    expected_revision: Some(0),
                    options: EdgeOptions::default(),
                },
            )
            .is_err()
        );
        assert!(
            runtime
                .command(
                    Command::Stop {
                        id: original.identity(),
                        expected_revision: Some(0),
                    },
                    &mut studio,
                    &mut engine,
                )
                .is_err()
        );
        assert!(
            start(
                &mut runtime,
                &mut studio,
                &mut engine,
                StartSpec {
                    source_id: &original.id,
                    target_pid: 202,
                    name: "Stale CAS",
                    expected_revision: Some(1),
                    options: EdgeOptions::default(),
                },
            )
            .is_err()
        );

        assert_eq!(roster(&runtime), before_roster);
        assert_eq!(studio.state(false).edges, before_edges);
        assert_eq!(engine.add_calls, 0);
        assert!(engine.remove_calls.is_empty());
        assert_eq!(engine.release_all_calls, 0);
        assert_eq!(engine.input_calls, 0);
    }

    #[test]
    fn clone_stop_retries_partial_native_release_at_the_same_revision() {
        let (mut runtime, mut studio, mut engine, original) = runtimes();
        let started = start(
            &mut runtime,
            &mut studio,
            &mut engine,
            StartSpec {
                source_id: &original.id,
                target_pid: 202,
                name: "Retry Clone",
                expected_revision: Some(0),
                options: EdgeOptions::default(),
            },
        )
        .unwrap();
        let clone = &started.slots[1].seat;
        let clone_keyboard = resolved(clone).keyboard.unwrap();
        let original_keyboard = resolved(&original).keyboard.unwrap();
        let before = studio.state(false);
        for (device, key) in [
            (clone_keyboard, 0x41),
            (clone_keyboard, 0x42),
            (original_keyboard, 0x41),
        ] {
            engine.input(Packet::key(device, key, false)).unwrap();
        }
        let stop = || Command::Stop {
            id: clone.identity(),
            expected_revision: Some(started.revision),
        };

        engine.failing_release = Some((clone_keyboard, 0x42));
        let error = runtime
            .command(stop(), &mut studio, &mut engine)
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("injected native release failure")
        );
        assert_eq!(runtime.state().revision, started.revision);
        assert!(runtime.state().slots[1].active);
        assert!(studio.output_state().users[1].active);
        assert!(engine.active[1]);
        assert_eq!(studio.state(false).edges, before.edges);
        assert_eq!(studio.state(false).graph_revision, before.graph_revision);
        assert_eq!(engine.released_devices, [clone_keyboard]);
        assert!(engine.key_held(clone_keyboard, 0x42));
        assert!(engine.key_held(original_keyboard, 0x41));
        assert!(engine.remove_calls.is_empty());

        engine.failing_release = None;
        let stopped = runtime.command(stop(), &mut studio, &mut engine).unwrap();
        assert!(stopped.acknowledged);
        assert_eq!(stopped.revision, started.revision + 1);
        assert_eq!(stopped.slots[1].seat.identity(), clone.identity());
        assert!(!stopped.slots[1].active);
        assert!(!studio.output_state().users[1].active);
        assert!(!engine.active[1]);
        assert!(studio.state(false).edges.is_empty());
        assert_eq!(
            studio.state(false).graph_revision,
            before.graph_revision + 1
        );
        assert_eq!(engine.released_devices, [clone_keyboard, clone_keyboard]);
        assert!(!engine.key_held(clone_keyboard, 0x42));
        assert!(engine.key_held(original_keyboard, 0x41));
        assert!(runtime.command(stop(), &mut studio, &mut engine).is_err());
        assert_eq!(engine.remove_calls, [1]);
        assert_eq!(engine.release_all_calls, 0);
    }

    #[test]
    fn start_stop_and_restart_append_fresh_slots_without_releasing_other_users() {
        let (mut runtime, mut studio, mut engine, original) = runtimes();
        let original_resolved = resolved(&original);
        let held_key = 0x41;
        engine
            .input(Packet::key(
                original_resolved.keyboard.unwrap(),
                held_key,
                false,
            ))
            .unwrap();
        assert!(engine.key_held(original_resolved.keyboard.unwrap(), held_key));

        let started = start(
            &mut runtime,
            &mut studio,
            &mut engine,
            StartSpec {
                source_id: &original.id,
                target_pid: 202,
                name: "Clone One",
                expected_revision: Some(0),
                options: EdgeOptions::default(),
            },
        )
        .unwrap();
        let first = started.slots[1].clone();
        assert_eq!(first.user, 1);
        assert!(first.active);
        assert_eq!(first.clone_of.as_deref(), Some(original.id.as_str()));
        assert_ne!(first.seat.identity(), original.identity());
        assert_ne!(first.seat.mouse, original.mouse);
        assert_ne!(first.seat.keyboard, original.keyboard);
        assert_eq!(engine.add_calls, 1);
        assert_eq!(engine.release_all_calls, 0);
        assert_eq!(engine.input_calls, 1);
        assert_eq!(studio.state(false).edges.len(), 1);

        let stopped = runtime
            .command(
                Command::Stop {
                    id: first.seat.identity(),
                    expected_revision: Some(started.revision),
                },
                &mut studio,
                &mut engine,
            )
            .unwrap();
        assert_eq!(stopped.slots.len(), 2);
        assert!(!stopped.slots[1].active);
        assert_eq!(stopped.slots[1].seat.identity(), first.seat.identity());
        assert_eq!(engine.remove_calls, vec![1]);
        assert_eq!(engine.release_all_calls, 0);
        assert_eq!(engine.input_calls, 1);
        assert!(engine.key_held(original_resolved.keyboard.unwrap(), held_key));
        assert!(
            !engine
                .released_devices
                .contains(&original_resolved.keyboard.unwrap())
        );

        assert!(runtime.require_active(1).is_err());
        assert!(runtime.require_device(resolved(&first.seat).mouse).is_err());

        let restarted = start(
            &mut runtime,
            &mut studio,
            &mut engine,
            StartSpec {
                source_id: &original.id,
                target_pid: 303,
                name: "Clone Two",
                expected_revision: Some(stopped.revision),
                options: EdgeOptions::default(),
            },
        )
        .unwrap();
        let second = &restarted.slots[2];
        assert_eq!(second.user, 2);
        assert!(second.active);
        assert_ne!(second.seat.identity(), first.seat.identity());
        assert_ne!(second.seat.mouse, first.seat.mouse);
        assert_ne!(second.seat.keyboard, first.seat.keyboard);
        assert_eq!(restarted.slots.len(), 3);
        assert_eq!(engine.add_calls, 2);
        let edges = studio.state(false).edges;
        assert_eq!(
            edges.len(),
            1,
            "The retired clone's incident edge must stay removed"
        );
        assert_eq!((edges[0].source, edges[0].target), (0, 2));
        assert_eq!(engine.input_calls, 1);
    }

    #[test]
    fn invalid_replication_options_fail_before_router_mutation() {
        let (mut runtime, mut studio, mut engine, original) = runtimes();
        let before_roster = roster(&runtime);
        let before_edges = studio.state(false).edges;
        let invalid_options = EdgeOptions {
            offset_x: 8,
            ..EdgeOptions::default()
        };
        assert!(
            start(
                &mut runtime,
                &mut studio,
                &mut engine,
                StartSpec {
                    source_id: &original.id,
                    target_pid: 202,
                    name: "Invalid edge",
                    expected_revision: Some(0),
                    options: invalid_options,
                },
            )
            .is_err()
        );
        assert_eq!(roster(&runtime), before_roster);
        assert_eq!(studio.state(false).edges, before_edges);
        assert_eq!(engine.add_calls, 0);
        assert!(engine.remove_calls.is_empty());
        assert_eq!(engine.release_all_calls, 0);
        assert_eq!(engine.input_calls, 0);
    }
}
