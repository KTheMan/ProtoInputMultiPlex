//! Explicit encodings for the pinned supervised, virtual-only ProtoInput pipe ABI.
//!
//! Header and body are deliberately separate: the resident receiver performs one
//! exact-sized read for each. Padding is zeroed, including C++'s one-byte empty
//! StopFocus structure. No Rust object representation is sent over the pipe.
use anyhow::{Result, ensure};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Frame {
    pub header: [u8; 8],
    pub body: Vec<u8>,
}

/// Only commands admitted by the current resident virtual lifecycle whitelist.
#[derive(Debug, Clone)]
pub(crate) enum Command {
    Hook {
        hook: u32,
        install: bool,
    },
    Filter {
        filter: u32,
        enabled: bool,
    },
    Block {
        message: u32,
        enabled: bool,
    },
    Window {
        hwnd: u64,
    },
    Focus {
        milliseconds: i32,
        activate: bool,
        activate_app: bool,
        nc_activate: bool,
        set_focus: bool,
        mouse_activate: bool,
    },
    StopFocus,
    State {
        index: i32,
    },
    Messages {
        wheel: bool,
        buttons: bool,
        motion: bool,
        keyboard: bool,
    },
    Draw {
        enabled: bool,
    },
    Freeze {
        enabled: bool,
    },
    Devices {
        mouse: Option<u32>,
        keyboard: Option<u32>,
    },
    Bounds {
        allow: bool,
        extend: bool,
    },
    Input {
        device: u32,
        kind: u32,
        flags: u32,
        x: i32,
        y: i32,
        data: u32,
    },
    Fence {
        hwnd: u64,
        event: u64,
        cookie: u64,
    },
    CursorStart {
        mapping: u64,
        mutex: u64,
        attachment: u64,
    },
    CursorAssociate {
        hwnd: u64,
        association: u64,
    },
    CursorStop {
        attachment: u64,
    },
    Begin {
        creation: u64,
        generation: u64,
        session: u64,
        commit: u64,
        reply: u64,
        event: u64,
    },
    BeginChecked {
        creation: u64,
        generation: u64,
        session: u64,
        commit: u64,
        reply: u64,
        event: u64,
        config_mapping: u64,
        config_event: u64,
    },
    InstallMessageFilter {
        creation: u64,
        generation: u64,
        session: u64,
        request: u64,
    },
    Retire {
        generation: u64,
        session: u64,
        reply: u64,
        event: u64,
    },
    Keyboard {
        epoch: u64,
        action: u32,
        mapping: u64,
        event: u64,
    },
}

impl Command {
    /// Validate and encode before starting any write or transferring remote handles.
    /// Target-specific handle narrowing is checked by `encode_for`.
    pub(crate) fn encode(&self) -> Result<Frame> {
        let (id, body) = match *self {
            Self::Hook { hook, install } => {
                ensure!((1..=10).contains(&hook), "unsupported virtual hook ID");
                (0, padded_bool(hook, install))
            }
            Self::Filter { filter, enabled } => {
                ensure!(filter <= 7, "invalid message filter ID");
                (2, padded_bool(filter, enabled))
            }
            Self::Block { message, enabled } => (3, padded_bool(message, enabled)),
            Self::Window { hwnd } => (4, u64s(&[hwnd])),
            Self::Focus {
                milliseconds,
                activate,
                activate_app,
                nc_activate,
                set_focus,
                mouse_activate,
            } => {
                ensure!(milliseconds > 0, "focus loop interval must be positive");
                let mut body = vec![0; 12];
                body[..4].copy_from_slice(&milliseconds.to_le_bytes());
                body[4..9].copy_from_slice(&[
                    activate.into(),
                    activate_app.into(),
                    nc_activate.into(),
                    set_focus.into(),
                    mouse_activate.into(),
                ]);
                (5, body)
            }
            Self::State { index } => {
                ensure!(index >= 1, "instance index must start at one");
                (6, index.to_le_bytes().to_vec())
            }
            Self::Messages {
                wheel,
                buttons,
                motion,
                keyboard,
            } => (
                7,
                vec![wheel.into(), buttons.into(), motion.into(), keyboard.into()],
            ),
            Self::Draw { enabled } => (8, vec![enabled.into()]),
            Self::Freeze { enabled } => (9, vec![enabled.into()]),
            Self::Devices { mouse, keyboard } => {
                ensure!(
                    mouse.is_some() || keyboard.is_some(),
                    "no virtual device selected"
                );
                if let Some(mouse) = mouse {
                    validate_device(mouse, false)?;
                }
                if let Some(keyboard) = keyboard {
                    validate_device(keyboard, true)?;
                }
                let mut body = Vec::with_capacity(8);
                body.extend_from_slice(&mouse.unwrap_or(u32::MAX).to_le_bytes());
                body.extend_from_slice(&keyboard.unwrap_or(u32::MAX).to_le_bytes());
                (10, body)
            }
            Self::StopFocus => (14, vec![0]),
            Self::Bounds { allow, extend } => (21, vec![allow.into(), extend.into()]),
            Self::Input {
                device,
                kind,
                flags,
                x,
                y,
                data,
            } => {
                mux_core::emulated::Packet {
                    device,
                    kind,
                    flags,
                    x,
                    y,
                    data,
                }
                .validate()?;
                let mut body = Vec::with_capacity(24);
                for value in [device, kind, flags, x as u32, y as u32, data] {
                    body.extend_from_slice(&value.to_le_bytes());
                }
                (25, body)
            }
            Self::Fence {
                hwnd,
                event,
                cookie,
            } => {
                nonzero(&[hwnd, event, cookie])?;
                (26, u64s(&[hwnd, event, cookie]))
            }
            Self::CursorStart {
                mapping,
                mutex,
                attachment,
            } => {
                nonzero(&[mapping, mutex, attachment])?;
                ensure!(mapping != mutex, "cursor handles must be distinct");
                (27, u64s(&[mapping, mutex, attachment]))
            }
            Self::CursorAssociate { hwnd, association } => {
                nonzero(&[hwnd, association])?;
                (28, u64s(&[hwnd, association]))
            }
            Self::CursorStop { attachment } => {
                nonzero(&[attachment])?;
                (29, u64s(&[attachment]))
            }
            Self::Begin {
                creation,
                generation,
                session,
                commit,
                reply,
                event,
            } => {
                nonzero(&[creation, generation, session, commit, reply, event])?;
                distinct(&[commit, reply, event])?;
                (
                    30,
                    u64s(&[creation, generation, session, commit, reply, event]),
                )
            }
            Self::Retire {
                generation,
                session,
                reply,
                event,
            } => {
                nonzero(&[generation, session, reply, event])?;
                distinct(&[reply, event])?;
                (31, u64s(&[generation, session, reply, event]))
            }
            Self::BeginChecked {
                creation,
                generation,
                session,
                commit,
                reply,
                event,
                config_mapping,
                config_event,
            } => {
                nonzero(&[
                    creation,
                    generation,
                    session,
                    commit,
                    reply,
                    event,
                    config_mapping,
                    config_event,
                ])?;
                distinct(&[commit, reply, event, config_mapping, config_event])?;
                (
                    33,
                    u64s(&[
                        creation,
                        generation,
                        session,
                        commit,
                        reply,
                        event,
                        config_mapping,
                        config_event,
                    ]),
                )
            }
            Self::InstallMessageFilter {
                creation,
                generation,
                session,
                request,
            } => {
                nonzero(&[creation, generation, session, request])?;
                let mut body = u64s(&[creation, generation, session, request]);
                body.extend_from_slice(&2u32.to_le_bytes());
                body.extend_from_slice(&1u32.to_le_bytes());
                body.extend_from_slice(&0u64.to_le_bytes());
                (34, body)
            }
            Self::Keyboard {
                epoch,
                action,
                mapping,
                event,
            } => {
                nonzero(&[epoch, mapping, event])?;
                ensure!(matches!(action, 1 | 2), "invalid keyboard-state action");
                distinct(&[mapping, event])?;
                let mut body = u64s(&[epoch, mapping, event]);
                body.extend_from_slice(&action.to_le_bytes());
                body.extend_from_slice(&0u32.to_le_bytes());
                (32, body)
            }
        };
        let mut header = [0; 8];
        header[..4].copy_from_slice(&u32::to_le_bytes(id));
        header[4..].copy_from_slice(&(body.len() as u32).to_le_bytes());
        Ok(Frame { header, body })
    }

    pub(crate) fn encode_for(&self, target_bits: u32) -> Result<Frame> {
        ensure!(
            matches!(target_bits, 32 | 64),
            "unsupported target architecture"
        );
        if target_bits == 32 {
            let handles: &[u64] = match self {
                Self::Window { hwnd } | Self::CursorAssociate { hwnd, .. } => &[*hwnd],
                Self::Fence { hwnd, event, .. } => &[*hwnd, *event],
                Self::CursorStart { mapping, mutex, .. } => &[*mapping, *mutex],
                Self::Keyboard { mapping, event, .. } => &[*mapping, *event],
                Self::Begin {
                    commit,
                    reply,
                    event,
                    ..
                } => &[*commit, *reply, *event],
                Self::Retire { reply, event, .. } => &[*reply, *event],
                Self::BeginChecked {
                    commit,
                    reply,
                    event,
                    config_mapping,
                    config_event,
                    ..
                } => &[*commit, *reply, *event, *config_mapping, *config_event],
                _ => &[],
            };
            ensure!(
                handles.iter().all(|handle| *handle <= u32::MAX.into()),
                "target handle exceeds 32 bits"
            );
        }
        self.encode()
    }
}

fn validate_device(device: u32, keyboard: bool) -> Result<()> {
    ensure!(
        device & mux_core::emulated::MASK == mux_core::emulated::BASE
            && device & 0x00ff_ffff >= 2
            && device & 1 == u32::from(keyboard),
        "invalid virtual device identity"
    );
    Ok(())
}

fn padded_bool(value: u32, enabled: bool) -> Vec<u8> {
    let mut body = vec![0; 8];
    body[..4].copy_from_slice(&value.to_le_bytes());
    body[4] = enabled.into();
    body
}

fn u64s(values: &[u64]) -> Vec<u8> {
    values
        .iter()
        .flat_map(|value| value.to_le_bytes())
        .collect()
}

fn nonzero(values: &[u64]) -> Result<()> {
    ensure!(
        values.iter().all(|value| *value != 0),
        "zero wire identity or handle"
    );
    Ok(())
}

fn distinct(values: &[u64]) -> Result<()> {
    for (index, value) in values.iter().enumerate() {
        ensure!(
            !values[..index].contains(value),
            "wire handles must be distinct"
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn golden(command: Command, id: u32, expected: &[u8]) {
        let frame = command.encode().unwrap();
        let mut header = id.to_le_bytes().to_vec();
        header.extend_from_slice(&(expected.len() as u32).to_le_bytes());
        assert_eq!(frame.header.as_slice(), header);
        assert_eq!(frame.body, expected);
    }

    #[test]
    fn checked_setup_matches_native_layout_and_rejects_handle_aliases() {
        use std::fmt::Write;
        let commands = [
            (
                "begin-checked",
                Command::BeginChecked {
                    creation: 7,
                    generation: 8,
                    session: 9,
                    commit: 0x11,
                    reply: 0x22,
                    event: 0x33,
                    config_mapping: 0x44,
                    config_event: 0x55,
                },
            ),
            (
                "install-checked",
                Command::InstallMessageFilter {
                    creation: 7,
                    generation: 8,
                    session: 9,
                    request: 1,
                },
            ),
        ];
        let mut actual = String::new();
        for (name, command) in commands {
            let frame = command.encode_for(32).unwrap();
            write!(actual, "{name}=").unwrap();
            for byte in frame.header.iter().chain(&frame.body) {
                write!(actual, "{byte:02x}").unwrap();
            }
            actual.push('\n');
        }
        let golden = include_str!("../../../../scripts/native/setup-ack-golden.txt")
            .lines()
            .take(2)
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(actual.trim_end(), golden);
        assert!(
            Command::BeginChecked {
                creation: 7,
                generation: 8,
                session: 9,
                commit: 1,
                reply: 2,
                event: 3,
                config_mapping: 4,
                config_event: 4
            }
            .encode()
            .is_err()
        );
        assert!(
            Command::BeginChecked {
                creation: 7,
                generation: 8,
                session: 9,
                commit: 1,
                reply: 2,
                event: 3,
                config_mapping: 4,
                config_event: 1u64 << 32
            }
            .encode_for(32)
            .is_err()
        );
        assert!(
            Command::InstallMessageFilter {
                creation: 7,
                generation: 0,
                session: 9,
                request: 1
            }
            .encode()
            .is_err()
        );
    }

    #[test]
    fn matches_shared_native_layout_and_byte_fixture() {
        use std::fmt::Write;
        let token = 0x8877_6655_4433_2211;
        let cases = [
            Command::Hook {
                hook: 10,
                install: true,
            },
            Command::Filter {
                filter: 7,
                enabled: false,
            },
            Command::Block {
                message: 0x2a3,
                enabled: true,
            },
            Command::Window { hwnd: token },
            Command::Focus {
                milliseconds: 10,
                activate: true,
                activate_app: false,
                nc_activate: true,
                set_focus: false,
                mouse_activate: true,
            },
            Command::State { index: 2 },
            Command::Messages {
                wheel: true,
                buttons: false,
                motion: true,
                keyboard: false,
            },
            Command::Draw { enabled: false },
            Command::Freeze { enabled: true },
            Command::Devices {
                mouse: Some(0xf1000002),
                keyboard: None,
            },
            Command::Devices {
                mouse: None,
                keyboard: Some(0xf1000003),
            },
            Command::StopFocus,
            Command::Bounds {
                allow: false,
                extend: true,
            },
            Command::Input {
                device: 0xf1000002,
                kind: 0,
                flags: 0,
                x: -2,
                y: 32767,
                data: 0,
            },
            Command::Input {
                device: 0xf1000003,
                kind: 2,
                flags: 0,
                x: 0,
                y: 0,
                data: 0xd83d,
            },
            Command::Fence {
                hwnd: 1,
                event: 2,
                cookie: token,
            },
            Command::CursorStart {
                mapping: 1,
                mutex: 2,
                attachment: token,
            },
            Command::CursorAssociate {
                hwnd: 1,
                association: token,
            },
            Command::CursorStop { attachment: token },
            Command::Begin {
                creation: token,
                generation: 2,
                session: 3,
                commit: 4,
                reply: 5,
                event: 6,
            },
            Command::Retire {
                generation: token,
                session: 2,
                reply: 3,
                event: 4,
            },
            Command::Keyboard {
                epoch: token,
                action: 2,
                mapping: 1,
                event: 2,
            },
        ];
        let mut actual = String::new();
        for command in cases {
            let frame = command.encode().unwrap();
            write!(
                actual,
                "{}=",
                u32::from_le_bytes(frame.header[..4].try_into().unwrap())
            )
            .unwrap();
            for byte in frame.header {
                write!(actual, "{byte:02x}").unwrap();
            }
            actual.push(':');
            for byte in frame.body {
                write!(actual, "{byte:02x}").unwrap();
            }
            actual.push('\n');
        }
        let expected =
            include_str!("../../../../scripts/native/wire-abi-golden.txt").replace("\r\n", "\n");
        assert_eq!(actual.trim_end(), expected.trim_end());
    }

    #[test]
    fn native_padding_and_bool_order_are_deterministic() {
        golden(
            Command::Hook {
                hook: 10,
                install: true,
            },
            0,
            &[10, 0, 0, 0, 1, 0, 0, 0],
        );
        golden(
            Command::Filter {
                filter: 7,
                enabled: false,
            },
            2,
            &[7, 0, 0, 0, 0, 0, 0, 0],
        );
        golden(
            Command::Block {
                message: 0x2a3,
                enabled: true,
            },
            3,
            &[0xa3, 2, 0, 0, 1, 0, 0, 0],
        );
        golden(
            Command::Focus {
                milliseconds: 10,
                activate: true,
                activate_app: false,
                nc_activate: true,
                set_focus: false,
                mouse_activate: true,
            },
            5,
            &[10, 0, 0, 0, 1, 0, 1, 0, 1, 0, 0, 0],
        );
        golden(Command::StopFocus, 14, &[0]);
        golden(Command::State { index: 2 }, 6, &[2, 0, 0, 0]);
        golden(
            Command::Messages {
                wheel: true,
                buttons: false,
                motion: true,
                keyboard: false,
            },
            7,
            &[1, 0, 1, 0],
        );
        golden(Command::Draw { enabled: false }, 8, &[0]);
        golden(Command::Freeze { enabled: true }, 9, &[1]);
        golden(
            Command::Bounds {
                allow: false,
                extend: true,
            },
            21,
            &[0, 1],
        );
    }

    #[test]
    fn device_sentinels_and_signed_packet_fields_match_native() {
        golden(
            Command::Devices {
                mouse: Some(0xf1000002),
                keyboard: None,
            },
            10,
            &[2, 0, 0, 0xf1, 0xff, 0xff, 0xff, 0xff],
        );
        golden(
            Command::Devices {
                mouse: None,
                keyboard: Some(0xf1000003),
            },
            10,
            &[0xff, 0xff, 0xff, 0xff, 3, 0, 0, 0xf1],
        );
        golden(
            Command::Input {
                device: 0xf1000002,
                kind: 0,
                flags: 0,
                x: -2,
                y: 32767,
                data: 0,
            },
            25,
            &[
                2, 0, 0, 0xf1, 0, 0, 0, 0, 0, 0, 0, 0, 0xfe, 0xff, 0xff, 0xff, 0xff, 0x7f, 0, 0, 0,
                0, 0, 0,
            ],
        );
        golden(
            Command::Input {
                device: 0xf1000003,
                kind: 2,
                flags: 0,
                x: 0,
                y: 0,
                data: 0xd83d,
            },
            25,
            &[
                3, 0, 0, 0xf1, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x3d, 0xd8, 0, 0,
            ],
        );
    }

    #[test]
    fn fixed_width_control_packets_preserve_order_and_tokens() {
        let token = 0x8877_6655_4433_2211;
        golden(
            Command::Window { hwnd: token },
            4,
            &[0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88],
        );
        for (command, id, fields) in [
            (
                Command::Fence {
                    hwnd: 1,
                    event: 2,
                    cookie: token,
                },
                26,
                vec![1, 2, token],
            ),
            (
                Command::CursorStart {
                    mapping: 1,
                    mutex: 2,
                    attachment: token,
                },
                27,
                vec![1, 2, token],
            ),
            (
                Command::CursorAssociate {
                    hwnd: 1,
                    association: token,
                },
                28,
                vec![1, token],
            ),
            (Command::CursorStop { attachment: token }, 29, vec![token]),
            (
                Command::Begin {
                    creation: token,
                    generation: 2,
                    session: 3,
                    commit: 4,
                    reply: 5,
                    event: 6,
                },
                30,
                vec![token, 2, 3, 4, 5, 6],
            ),
            (
                Command::Retire {
                    generation: token,
                    session: 2,
                    reply: 3,
                    event: 4,
                },
                31,
                vec![token, 2, 3, 4],
            ),
        ] {
            let expected: Vec<u8> = fields.into_iter().flat_map(u64::to_le_bytes).collect();
            golden(command, id, &expected);
        }
        golden(
            Command::Keyboard {
                epoch: token,
                action: 2,
                mapping: 1,
                event: 2,
            },
            32,
            &[
                0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 1, 0, 0, 0, 0, 0, 0, 0, 2, 0, 0, 0,
                0, 0, 0, 0, 2, 0, 0, 0, 0, 0, 0, 0,
            ],
        );
    }

    #[test]
    fn rejects_unsupported_configuration_before_transfer() {
        for command in [
            Command::Hook {
                hook: 0,
                install: true,
            },
            Command::Hook {
                hook: 11,
                install: true,
            },
            Command::Filter {
                filter: 8,
                enabled: true,
            },
            Command::State { index: 0 },
            Command::Focus {
                milliseconds: -1,
                activate: false,
                activate_app: false,
                nc_activate: false,
                set_focus: false,
                mouse_activate: false,
            },
            Command::Devices {
                mouse: Some(1234),
                keyboard: None,
            },
            Command::Devices {
                mouse: Some(0xf1000003),
                keyboard: None,
            },
            Command::Devices {
                mouse: None,
                keyboard: Some(0xf1000001),
            },
            Command::Devices {
                mouse: None,
                keyboard: None,
            },
            Command::Keyboard {
                epoch: 1,
                action: 3,
                mapping: 2,
                event: 3,
            },
            Command::Keyboard {
                epoch: 1,
                action: 1,
                mapping: 2,
                event: 2,
            },
            Command::Begin {
                creation: 1,
                generation: 2,
                session: 3,
                commit: 4,
                reply: 4,
                event: 5,
            },
            Command::Retire {
                generation: 0,
                session: 2,
                reply: 3,
                event: 4,
            },
            Command::Input {
                device: 0xf1000002,
                kind: 1,
                flags: 0,
                x: 30,
                y: 0,
                data: 65,
            },
            Command::Input {
                device: 0xf1000003,
                kind: 2,
                flags: 0,
                x: 0,
                y: 0,
                data: 65536,
            },
        ] {
            assert!(command.encode().is_err(), "accepted {command:?}");
        }
    }

    #[test]
    fn x86_narrows_handles_but_not_lifecycle_or_fence_tokens() {
        let wide = u64::from(u32::MAX) + 1;
        for command in [
            Command::Window { hwnd: wide },
            Command::Fence {
                hwnd: 1,
                event: wide,
                cookie: 1,
            },
            Command::CursorStart {
                mapping: wide,
                mutex: 1,
                attachment: 1,
            },
            Command::CursorAssociate {
                hwnd: wide,
                association: 1,
            },
            Command::Keyboard {
                epoch: 1,
                action: 1,
                mapping: wide,
                event: 1,
            },
            Command::Begin {
                creation: 1,
                generation: 2,
                session: 3,
                commit: wide,
                reply: 1,
                event: 2,
            },
            Command::Retire {
                generation: 1,
                session: 2,
                reply: 1,
                event: wide,
            },
        ] {
            assert!(command.encode_for(32).is_err());
            assert!(command.encode_for(64).is_ok());
        }
        assert!(
            Command::Begin {
                creation: wide,
                generation: wide,
                session: wide,
                commit: 1,
                reply: 2,
                event: 3
            }
            .encode_for(32)
            .is_ok()
        );
        assert!(
            Command::Fence {
                hwnd: 1,
                event: 2,
                cookie: wide
            }
            .encode_for(32)
            .is_ok()
        );
        assert!(Command::Window { hwnd: 0 }.encode_for(32).is_ok());
        assert!(Command::StopFocus.encode_for(16).is_err());
    }
}
