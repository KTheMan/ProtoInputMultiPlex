//! Portable configuration and independent pointer state. No OS input is synthesized here.
use anyhow::{Result, bail, ensure};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};
pub mod annotation;
pub mod application_event;
pub mod automation;
pub mod desktop;
pub mod emulated;
pub mod forwarding;
pub mod keyboard;
pub mod macro_authoring;
pub mod magnifier;
pub mod mapper;
pub mod motion_coalescing;
pub mod output;
pub mod replication;
pub mod routing;
pub mod screen;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeviceKind {
    Mouse,
    Keyboard,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Device {
    /// Raw Input interface path, persisted instead of the ephemeral Windows handle.
    pub path: String,
    pub kind: DeviceKind,
    pub handle: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Seat {
    /// Stable identity for profile references. Empty values from legacy profiles use a
    /// deterministic identity derived from the persisted mouse interface path.
    #[serde(default)]
    pub id: String,
    pub name: String,
    pub mouse: String,
    pub keyboard: Option<String>,
    #[serde(default)]
    pub target_pid: u32,
    #[serde(default)]
    pub desktop_start: Option<[i32; 2]>,
    #[serde(default)]
    pub cursor: desktop::CursorSettings,
    #[serde(default)]
    pub annotation: annotation::Settings,
    #[serde(default)]
    pub magnifier: magnifier::Settings,
    #[serde(default)]
    pub output: output::Settings,
}

impl Seat {
    /// Returns the persisted ID, or a deterministic opaque ID for a legacy profile.
    pub fn identity(&self) -> String {
        if !self.id.is_empty() {
            self.id.clone()
        } else {
            legacy_seat_id(&self.mouse)
        }
    }

    /// Creates a fresh opaque ID for a newly-created seat.
    pub fn new_id() -> String {
        static NEXT_ID: AtomicU64 = AtomicU64::new(1);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let sequence = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        format!(
            "seat-{:08x}-{nanos:032x}-{sequence:016x}",
            std::process::id()
        )
    }
}

fn legacy_seat_id(mouse_path: &str) -> String {
    // Keep migration deterministic without depending on Rust's randomized/default hasher.
    let normalized = mouse_path.to_lowercase();
    let mut a = 0xcbf29ce484222325_u64;
    let mut b = 0x84222325cbf29ce4_u64;
    for byte in normalized.as_bytes() {
        a = (a ^ u64::from(*byte)).wrapping_mul(0x100000001b3);
        b = (b ^ u64::from(*byte).wrapping_add(0x9d))
            .rotate_left(7)
            .wrapping_mul(0x9e3779b185ebca87);
    }
    format!("legacy-{a:016x}{b:016x}")
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    pub version: u32,
    #[serde(default)]
    pub concurrent: bool,
    #[serde(default)]
    pub desktop: Option<routing::DesktopRouting>,
    #[serde(default)]
    pub replication: replication::Definition,
    #[serde(default)]
    pub forwarding: forwarding::Definition,
    #[serde(default, skip_serializing_if = "empty_mapper")]
    pub mapper: mapper::Definition,
    pub seats: Vec<Seat>,
}

fn empty_mapper(value: &mapper::Definition) -> bool {
    value.version == 1 && value.nodes.is_empty() && value.edges.is_empty()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedSeat {
    pub name: String,
    pub mouse: u32,
    pub keyboard: Option<u32>,
    pub target_pid: u32,
}

impl Default for Profile {
    fn default() -> Self {
        Self {
            version: 1,
            concurrent: false,
            desktop: None,
            replication: replication::Definition::default(),
            forwarding: forwarding::Definition::default(),
            mapper: mapper::Definition::default(),
            seats: Vec::new(),
        }
    }
}

impl Profile {
    /// Materializes deterministic IDs on legacy seats before saving a profile.
    pub fn normalize_ids(&mut self) -> Result<()> {
        for seat in &mut self.seats {
            if seat.id.is_empty() {
                seat.id = legacy_seat_id(&seat.mouse);
            }
        }
        self.validate()
    }

    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.version == 1,
            "unsupported profile version {}",
            self.version
        );
        let (mut names, mut devices, mut pids, mut identities) = (
            HashSet::new(),
            HashSet::new(),
            HashSet::new(),
            HashSet::new(),
        );
        for seat in &self.seats {
            ensure!(seat.id.len() <= 128, "seat ID is too long");
            if !seat.id.is_empty() {
                ensure!(
                    !seat.id.trim().is_empty()
                        && seat.id.trim() == seat.id
                        && !seat.id.chars().any(char::is_control),
                    "seat ID contains whitespace padding or control characters"
                );
            }
            ensure!(
                identities.insert(seat.identity()),
                "duplicate seat identity"
            );
            seat.cursor.validate()?;
            seat.cursor
                .keyboard
                .validate_application_route(self.desktop.is_some())?;
            if seat.cursor.keyboard.application_focus == keyboard::ApplicationFocus::NativeControls
            {
                ensure!(
                    emulated::parse(&seat.mouse)?
                        .is_some_and(|device| device.kind == DeviceKind::Mouse)
                        && seat
                            .keyboard
                            .as_deref()
                            .map(emulated::parse)
                            .transpose()?
                            .flatten()
                            .is_some_and(|device| device.kind == DeviceKind::Keyboard),
                    "Native control focus requires a virtual mouse and paired virtual keyboard"
                );
            }
            ensure!(
                !seat.cursor.requires_extended_overlay()
                    || self.desktop.is_some()
                    || self.concurrent,
                "Extended cursor appearance and idle hiding require desktop cursor routing"
            );
            seat.output.validate()?;
            if seat.output != output::Settings::default() {
                let virtual_mouse = emulated::parse(&seat.mouse)?.is_some();
                let virtual_keyboard = seat
                    .keyboard
                    .as_ref()
                    .map(|path| emulated::parse(path))
                    .transpose()?
                    .flatten()
                    .is_some();
                ensure!(
                    virtual_mouse,
                    "Output conversion currently requires a virtual mouse user"
                );
                ensure!(
                    !seat.output.requires_keyboard() && seat.output.keys.is_empty()
                        || virtual_keyboard,
                    "Output key rules require a paired virtual keyboard"
                );
                ensure!(
                    self.desktop.is_some() || self.concurrent,
                    "Output conversion requires automatic desktop or concurrent assigned routing"
                );
            }
            ensure!(
                seat.cursor.horizontal_wheel != desktop::HorizontalWheelPolicy::Preserve
                    || self.desktop.is_some()
                    || self.concurrent,
                "Preserving horizontal scroll requires automatic desktop or concurrent assigned routing"
            );
            ensure!(
                seat.cursor.horizontal_wheel_emulation == desktop::HorizontalWheelEmulation::Never
                    || self.desktop.is_some()
                    || self.concurrent,
                "Horizontal wheel emulation requires automatic desktop or concurrent assigned routing"
            );
            if seat.cursor.horizontal_wheel_emulation != desktop::HorizontalWheelEmulation::Never {
                ensure!(
                    emulated::parse(&seat.mouse)?
                        .is_some_and(|device| device.kind == DeviceKind::Mouse),
                    "Horizontal wheel emulation currently requires a virtual mouse"
                );
            }
            seat.annotation.validate()?;
            seat.magnifier.validate()?;
            ensure!(
                self.desktop.is_some()
                    || self.concurrent
                    || seat.magnifier.mode == magnifier::Mode::Off,
                "Magnifier requires automatic desktop or concurrent assigned routing"
            );
            ensure!(
                seat.annotation.clear_key.is_none() || seat.keyboard.is_some(),
                "annotation clear shortcut requires a paired keyboard"
            );
            ensure!(
                self.desktop.is_some()
                    || self.concurrent
                    || seat.annotation.tool == annotation::Tool::Off,
                "Annotation requires automatic desktop or concurrent assigned routing"
            );
            ensure!(!seat.name.trim().is_empty(), "seat name is empty");
            ensure!(
                names.insert(seat.name.to_lowercase()),
                "duplicate seat name: {}",
                seat.name
            );
            if self.desktop.is_none() {
                ensure!(seat.target_pid != 0, "{} needs a target PID", seat.name);
                ensure!(
                    pids.insert(seat.target_pid),
                    "each seat needs its own process; duplicate PID {}",
                    seat.target_pid
                );
            }
            for path in std::iter::once(&seat.mouse).chain(seat.keyboard.iter()) {
                ensure!(!path.is_empty(), "{} has an empty device path", seat.name);
                ensure!(
                    devices.insert(path.to_lowercase()),
                    "device assigned more than once: {path}"
                );
            }
        }
        self.replication.resolve(&self.seats)?;
        self.mapper.validate()?;
        if !self.mapper.nodes.is_empty() {
            let mapper_seats: std::collections::BTreeMap<_, _> = self
                .seats
                .iter()
                .map(|seat| (seat.identity(), seat))
                .collect();
            let mapper_user = |user: &str, keyboard: bool| -> Result<()> {
                let seat = mapper_seats.get(user).ok_or_else(|| {
                    anyhow::anyhow!("Input Mapper references missing user {user}")
                })?;
                ensure!(
                    emulated::parse(&seat.mouse)?
                        .is_some_and(|device| device.kind == DeviceKind::Mouse),
                    "Input Mapper user {user} requires a virtual mouse"
                );
                if let Some(path) = &seat.keyboard {
                    ensure!(
                        emulated::parse(path)?
                            .is_some_and(|device| device.kind == DeviceKind::Keyboard),
                        "Input Mapper user {user} keyboard must be virtual"
                    );
                }
                ensure!(
                    !keyboard || seat.keyboard.is_some(),
                    "Input Mapper user {user} needs a paired virtual keyboard"
                );
                Ok(())
            };
            for node in &self.mapper.nodes {
                if let Some((source, _)) = node.kind.source() {
                    mapper_user(source.user(), matches!(source, mapper::Source::Key { .. }))?;
                }
                if let mapper::NodeKind::Sink { user, channel } = &node.kind {
                    mapper_user(
                        user,
                        matches!(
                            channel,
                            mapper::OutputChannel::KeyHold { .. }
                                | mapper::OutputChannel::KeyPulse { .. }
                        ),
                    )?;
                }
            }
        }
        let resolved_forwarding = self.forwarding.resolve(&self.seats)?;
        ensure!(
            self.replication.edges.is_empty() || self.forwarding.edges.is_empty(),
            "replication and input forwarding cannot both be enabled"
        );
        if !self.forwarding.edges.is_empty() {
            ensure!(
                self.desktop.is_some(),
                "input forwarding requires desktop routing"
            );
            let endpoints: HashSet<usize> = resolved_forwarding
                .iter()
                .flat_map(|edge| [edge.source, edge.target])
                .collect();
            for index in endpoints {
                let seat = &self.seats[index];
                ensure!(
                    emulated::parse(&seat.mouse)?
                        .is_some_and(|device| device.kind == DeviceKind::Mouse),
                    "input forwarding requires virtual mice for every seat"
                );
                ensure!(
                    seat.keyboard
                        .as_deref()
                        .map(emulated::parse)
                        .transpose()?
                        .flatten()
                        .is_some_and(|device| device.kind == DeviceKind::Keyboard)
                        || seat.keyboard.is_none(),
                    "input forwarding keyboard must be virtual"
                );
            }
        }
        ensure!(
            self.desktop.is_some()
                || self.concurrent
                || !self.replication.edges.iter().any(|edge| {
                    edge.mouse
                        && edge.options.motion
                        && (edge.options.position == replication::PositionMode::SourceRelative
                            || edge.options.step_interval_ms != 0)
                }),
            "Source-relative or paced pointer replication requires automatic desktop or concurrent assigned routing"
        );
        Ok(())
    }

    pub fn resolve(&self, devices: &[Device]) -> Result<Vec<ResolvedSeat>> {
        self.validate()?;
        let resolve = |path: &str, kind| -> Result<u32> {
            if let Some(virtual_device) = emulated::parse(path)? {
                ensure!(virtual_device.kind == kind, "emulated device kind mismatch");
                ensure!(
                    !devices
                        .iter()
                        .any(|d| d.handle == virtual_device.handle && d.path != path),
                    "emulated handle collides with another device"
                );
                return Ok(virtual_device.handle as u32);
            }
            let Some(device) = devices
                .iter()
                .find(|d| d.path.eq_ignore_ascii_case(path) && d.kind == kind)
            else {
                bail!("assigned {kind:?} is disconnected: {path}");
            };
            ensure!(device.handle != 0, "null device handles are not supported");
            u32::try_from(device.handle)
                .map_err(|_| anyhow::anyhow!("device handle exceeds ProtoInput's 32-bit API"))
        };
        self.seats
            .iter()
            .map(|seat| {
                Ok(ResolvedSeat {
                    name: seat.name.clone(),
                    mouse: resolve(&seat.mouse, DeviceKind::Mouse)?,
                    keyboard: seat
                        .keyboard
                        .as_deref()
                        .map(|p| resolve(p, DeviceKind::Keyboard))
                        .transpose()?,
                    target_pid: seat.target_pid,
                })
            })
            .collect()
    }
}

#[derive(Debug, Clone)]
pub struct Pointer {
    pub x: f32,
    pub y: f32,
    pub buttons: u16,
    pub packets: u64,
}
impl Default for Pointer {
    fn default() -> Self {
        Self {
            x: 0.5,
            y: 0.5,
            buttons: 0,
            packets: 0,
        }
    }
}
impl Pointer {
    pub fn update(&mut self, x: i32, y: i32, absolute: bool, button_flags: u16) {
        if absolute {
            self.x = x as f32 / 65535.0;
            self.y = y as f32 / 65535.0;
        } else {
            self.x += x as f32 / 1200.0;
            self.y += y as f32 / 800.0;
        }
        self.x = self.x.clamp(0.0, 1.0);
        self.y = self.y.clamp(0.0, 1.0);
        for i in 0..5 {
            if button_flags & (1 << (i * 2)) != 0 {
                self.buttons |= 1 << i;
            }
            if button_flags & (2 << (i * 2)) != 0 {
                self.buttons &= !(1 << i);
            }
        }
        self.packets = self.packets.saturating_add(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn legacy_profile_defaults_to_empty_mapper_and_does_not_accept_armed_runtime_state() {
        let mut document = serde_json::to_value(profile()).unwrap();
        document.as_object_mut().unwrap().remove("mapper");
        let restored: Profile = serde_json::from_value(document).unwrap();
        assert_eq!(restored.mapper, mapper::Definition::default());
        let mut saved = serde_json::to_value(restored).unwrap();
        assert!(
            saved.get("mapper").is_none(),
            "Ordinary profiles retain the prior writer shape"
        );
        saved["mapper"]["armed"] = true.into();
        assert!(serde_json::from_value::<Profile>(saved).is_err());
    }

    #[test]
    fn mapper_profile_requires_existing_virtual_users_and_paired_keyboards() {
        use mapper::{Definition, Disposition, Edge, Node, NodeKind, OutputChannel, PortRef};
        let mut value = profile();
        value.seats[0].mouse = "virtual://mouse/1".into();
        value.seats[0].keyboard = Some("virtual://keyboard/1".into());
        let user = value.seats[0].identity();
        value.mapper = Definition {
            version: 1,
            groups: Vec::new(),
            nodes: vec![
                Node {
                    id: "source".into(),
                    position: None,
                    kind: NodeKind::Key {
                        user: user.clone(),
                        key: 65,
                        disposition: Disposition::Passthrough,
                    },
                },
                Node {
                    id: "sink".into(),
                    position: None,
                    kind: NodeKind::Sink {
                        user,
                        channel: OutputChannel::KeyHold { key: 66 },
                    },
                },
            ],
            edges: vec![Edge {
                from: PortRef {
                    node: "source".into(),
                    port: "value".into(),
                },
                to: PortRef {
                    node: "sink".into(),
                    port: "value".into(),
                },
            }],
        };
        value.validate().unwrap();
        value.seats[0].keyboard = None;
        assert!(
            value
                .validate()
                .unwrap_err()
                .to_string()
                .contains("keyboard")
        );
        value.seats[0].keyboard = Some("virtual://keyboard/1".into());
        if let NodeKind::Sink { user, .. } = &mut value.mapper.nodes[1].kind {
            *user = "missing-user".into();
        }
        assert!(
            value
                .validate()
                .unwrap_err()
                .to_string()
                .contains("missing user")
        );
    }
    fn profile() -> Profile {
        Profile {
            version: 1,
            concurrent: false,
            desktop: None,
            replication: replication::Definition::default(),
            forwarding: forwarding::Definition::default(),
            mapper: mapper::Definition::default(),
            seats: vec![Seat {
                id: Seat::new_id(),
                name: "Blue".into(),
                mouse: "mouse-a".into(),
                keyboard: Some("key-a".into()),
                target_pid: 100,
                desktop_start: None,
                cursor: Default::default(),
                annotation: Default::default(),
                magnifier: Default::default(),
                output: Default::default(),
            }],
        }
    }
    fn devices() -> Vec<Device> {
        vec![
            Device {
                path: "mouse-a".into(),
                kind: DeviceKind::Mouse,
                handle: 123,
            },
            Device {
                path: "key-a".into(),
                kind: DeviceKind::Keyboard,
                handle: 456,
            },
        ]
    }
    #[test]
    fn native_control_focus_rejects_assigned_and_unpaired_profiles() {
        let mut p = profile();
        p.seats[0].cursor.keyboard.application_focus = keyboard::ApplicationFocus::NativeControls;
        assert!(p.validate().is_err());
        p.concurrent = true;
        assert!(
            p.validate().is_err(),
            "assigned concurrent routing is unsupported"
        );
        p.desktop = Some(routing::DesktopRouting::default());
        assert!(p.validate().is_err(), "physical devices are unsupported");
        p.seats[0].mouse = "virtual://mouse/10".into();
        assert!(p.validate().is_err(), "a virtual keyboard is required");
        p.seats[0].keyboard = Some("virtual://keyboard/10".into());
        assert!(p.validate().is_ok());
        let restored: Profile = serde_json::from_value(serde_json::to_value(&p).unwrap()).unwrap();
        assert!(restored.validate().is_ok());
        assert_eq!(
            restored.seats[0].cursor.keyboard.application_focus,
            keyboard::ApplicationFocus::NativeControls
        );
        p.seats[0].keyboard = None;
        assert!(p.validate().is_err());
    }

    #[test]
    fn resolves_current_handles() {
        let mut devices = devices();
        devices[0].handle = 789;
        assert_eq!(profile().resolve(&devices).unwrap()[0].mouse, 789);
    }
    #[test]
    fn horizontal_preservation_requires_a_desktop_cursor_route() {
        let mut p = profile();
        p.seats[0].cursor.horizontal_wheel = desktop::HorizontalWheelPolicy::Preserve;
        assert!(p.validate().is_err());
        p.concurrent = true;
        assert!(p.validate().is_ok());
        p.concurrent = false;
        p.desktop = Some(routing::DesktopRouting::default());
        assert!(p.validate().is_ok());
    }
    #[test]
    fn extended_appearance_requires_an_overlay_route_without_requiring_hardware() {
        let mut p = profile();
        p.seats[0].cursor.size = 256.;
        assert!(p.validate().is_err());
        p.concurrent = true;
        assert!(p.validate().is_ok());
        p.concurrent = false;
        p.desktop = Some(routing::DesktopRouting::default());
        assert!(p.validate().is_ok());
        p.desktop = None;
        p.seats[0].cursor.size = 28.;
        p.seats[0].cursor.idle_hide_ms = Some(1000);
        assert!(p.validate().is_err());
        p.seats[0].cursor.idle_hide_ms = None;
        assert!(p.validate().is_ok());
    }
    #[test]
    fn conditional_horizontal_emulation_requires_virtual_mouse_and_desktop_route() {
        let mut p = profile();
        p.seats[0].cursor.horizontal_wheel_emulation = desktop::HorizontalWheelEmulation::Always;
        assert!(p.validate().is_err(), "fixed physical route must reject it");

        p.concurrent = true;
        assert!(p.validate().is_err(), "physical mouse must still reject it");
        p.seats[0].mouse = "virtual://keyboard/11".into();
        assert!(p.validate().is_err(), "virtual keyboard is not a mouse");
        p.seats[0].mouse = "virtual://mouse/10".into();
        p.seats[0].keyboard = Some("virtual://keyboard/10".into());
        assert!(p.validate().is_ok());

        p.concurrent = false;
        p.desktop = Some(routing::DesktopRouting::default());
        assert!(p.validate().is_ok());
    }
    #[test]
    fn rejects_disconnected_or_wrong_kind() {
        assert!(profile().resolve(&[]).is_err());
        let mut d = devices();
        d[0].kind = DeviceKind::Keyboard;
        assert!(profile().resolve(&d).is_err());
    }
    #[test]
    fn rejects_conflicting_assignments() {
        let mut p = profile();
        let mut other = p.seats[0].clone();
        other.name = "Red".into();
        other.target_pid = 200;
        other.mouse = "MOUSE-A".into();
        other.keyboard = None;
        p.seats.push(other);
        assert!(p.validate().is_err());
        p.seats[1].mouse = "mouse-b".into();
        p.seats[1].target_pid = 100;
        assert!(p.validate().is_err());
    }
    #[test]
    fn rejects_unknown_schema() {
        let mut p = profile();
        p.version = 2;
        assert!(p.validate().is_err());
    }
    #[test]
    fn rejects_null_and_oversized_backend_handles() {
        let mut d = devices();
        d[0].handle = 0;
        assert!(profile().resolve(&d).is_err());
        if let Some(too_large) = (u32::MAX as usize).checked_add(1) {
            d[0].handle = too_large;
            assert!(profile().resolve(&d).is_err());
        }
    }
    #[test]
    fn profile_round_trip() {
        let p: Profile = serde_json::from_str(&serde_json::to_string(&profile()).unwrap()).unwrap();
        assert_eq!(p.resolve(&devices()).unwrap()[0].keyboard, Some(456));
    }

    #[test]
    fn forwarding_requires_virtual_endpoint_devices_and_desktop_mode() {
        let mut empty_invalid = profile();
        empty_invalid.forwarding.version = 2;
        assert!(empty_invalid.validate().is_err());
        let mut p = profile();
        p.forwarding.edges.push(forwarding::Edge {
            source: p.seats[0].identity(),
            target: "another-seat".into(),
            keyboard: true,
        });
        p.desktop = Some(routing::DesktopRouting::default());
        let mut other = p.seats[0].clone();
        other.id = "another-seat".into();
        other.name = "Red".into();
        other.mouse = "virtual://mouse/11".into();
        other.keyboard = Some("virtual://keyboard/11".into());
        other.target_pid = 200;
        p.seats[0].mouse = "virtual://mouse/10".into();
        p.seats[0].keyboard = Some("virtual://keyboard/10".into());
        p.seats.push(other);
        assert!(p.validate().is_ok());
        p.desktop = None;
        assert!(p.validate().is_err());
        p.desktop = Some(routing::DesktopRouting::default());
        p.seats[1].mouse = "physical-mouse".into();
        assert!(p.validate().is_err());
        p.seats[1].mouse = "virtual://mouse/11".into();
        p.seats[1].keyboard = None;
        assert!(
            p.validate().is_err(),
            "keyboard edge needs destination capability"
        );
    }

    #[test]
    fn legacy_profile_materializes_stable_seat_ids_and_default_graph() {
        let mut value = serde_json::to_value(profile()).unwrap();
        value.as_object_mut().unwrap().remove("replication");
        value.as_object_mut().unwrap().remove("forwarding");
        value["seats"][0].as_object_mut().unwrap().remove("id");
        let mut parsed: Profile = serde_json::from_value(value).unwrap();
        assert!(parsed.replication.edges.is_empty());
        assert!(parsed.forwarding.edges.is_empty());
        let before = parsed.seats[0].identity();
        parsed.normalize_ids().unwrap();
        assert_eq!(parsed.seats[0].id, before);
        let restored: Profile =
            serde_json::from_str(&serde_json::to_string(&parsed).unwrap()).unwrap();
        assert_eq!(restored.seats[0].identity(), before);
    }

    #[test]
    fn duplicate_explicit_seat_ids_are_rejected() {
        let mut p = profile();
        let mut other = p.seats[0].clone();
        other.name = "Red".into();
        other.mouse = "mouse-b".into();
        other.keyboard = None;
        other.target_pid = 200;
        p.seats[0].id = "same-id".into();
        other.id = "same-id".into();
        p.seats.push(other);
        assert!(p.validate().is_err());
    }

    #[test]
    fn whitespace_only_seat_id_is_rejected_without_legacy_migration() {
        let mut p = profile();
        p.seats[0].id = "   ".into();
        assert!(p.validate().is_err());
        assert!(p.normalize_ids().is_err());
        assert_eq!(p.seats[0].id, "   ");
    }

    #[test]
    fn pointers_are_independent_and_clamped() {
        let mut a = Pointer::default();
        let b = Pointer::default();
        a.update(100000, -100000, false, 1);
        assert_eq!((a.x, a.y, a.buttons), (1.0, 0.0, 1));
        assert_eq!((b.x, b.y, b.buttons), (0.5, 0.5, 0));
        a.update(32767, 65535, true, 2);
        assert!((a.x - 0.5).abs() < 0.001);
        assert_eq!((a.y, a.buttons), (1.0, 0));
    }
}
