//! Application-local emulated devices. These are not Windows HID devices.
use crate::{Device, DeviceKind};
use anyhow::{Result, ensure};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub const BASE: u32 = 0xF1000000;
pub const MASK: u32 = 0xFF000000;
pub fn device(kind: DeviceKind, id: u32) -> Result<Device> {
    ensure!(
        (1..=0x7FFFFF).contains(&id),
        "emulated device ID out of range"
    );
    let (name, bit) = match kind {
        DeviceKind::Mouse => ("mouse", 0),
        DeviceKind::Keyboard => ("keyboard", 1),
    };
    Ok(Device {
        path: format!("virtual://{name}/{id}"),
        kind,
        handle: (BASE | (id << 1) | bit) as usize,
    })
}
pub fn parse(path: &str) -> Result<Option<Device>> {
    let Some(suffix) = path.strip_prefix("virtual://") else {
        return Ok(None);
    };
    let (kind, id) = suffix
        .split_once('/')
        .ok_or_else(|| anyhow::anyhow!("invalid emulated device path"))?;
    let kind = match kind {
        "mouse" => DeviceKind::Mouse,
        "keyboard" => DeviceKind::Keyboard,
        _ => anyhow::bail!("invalid emulated device kind"),
    };
    let device = device(kind, id.parse()?)?;
    ensure!(
        path == device.path,
        "emulated device paths must use canonical decimal IDs"
    );
    Ok(Some(device))
}

/// Fixed-width native transport. All generation and validation live in Rust.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema)]
#[repr(C)]
pub struct Packet {
    pub device: u32,
    pub kind: u32,
    pub flags: u32,
    pub x: i32,
    pub y: i32,
    pub data: u32,
}
const _: () = {
    assert!(std::mem::size_of::<Packet>() == 24);
    assert!(std::mem::align_of::<Packet>() == 4);
};
impl Packet {
    pub fn motion(device: u32, x: i32, y: i32) -> Self {
        Self {
            device,
            kind: 0,
            flags: 0,
            x,
            y,
            data: 0,
        }
    }
    pub fn button(device: u32, button: u32, released: bool) -> Self {
        let flags = if button < 5 {
            1 << (button * 2 + u32::from(released))
        } else {
            u32::MAX
        };
        Self {
            flags,
            ..Self::motion(device, 0, 0)
        }
    }
    pub fn wheel(device: u32, delta: i16) -> Self {
        Self {
            flags: 0x400,
            data: delta as u16 as u32,
            ..Self::motion(device, 0, 0)
        }
    }
    pub fn horizontal_wheel(device: u32, delta: i16) -> Self {
        Self {
            flags: 0x800,
            data: delta as u16 as u32,
            ..Self::motion(device, 0, 0)
        }
    }
    pub fn key(device: u32, key: u32, released: bool) -> Self {
        let scan = match key {
            0x57 => 0x11,
            0x41 => 0x1e,
            0x53 => 0x1f,
            0x44 => 0x20,
            0x20 => 0x39,
            _ => 0,
        };
        Self::key_scanned(device, key, scan, 0, released)
    }
    pub fn key_scanned(device: u32, key: u32, scan: i32, extended: u32, released: bool) -> Self {
        Self {
            device,
            kind: 1,
            flags: extended | u32::from(released),
            x: scan,
            y: 0,
            data: key,
        }
    }
    pub fn text_unit(device: u32, unit: u16) -> Self {
        Self {
            device,
            kind: 2,
            flags: 0,
            x: 0,
            y: 0,
            data: u32::from(unit),
        }
    }
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.device & MASK == BASE && self.device & 0xFFFFFF >= 2,
            "not an emulated device"
        );
        ensure!(
            self.kind <= 2 && self.device & 1 == u32::from(self.kind != 0),
            "emulated device kind mismatch"
        );
        if self.kind == 0 {
            const BUTTON_FLAGS: u32 =
                0x001 | 0x002 | 0x004 | 0x008 | 0x010 | 0x020 | 0x040 | 0x080 | 0x100 | 0x200;
            ensure!(
                (self.flags == 0
                    || matches!(self.flags, 0x400 | 0x800)
                    || (self.flags & !BUTTON_FLAGS == 0 && self.flags.is_power_of_two()))
                    && (if matches!(self.flags, 0x400 | 0x800) {
                        self.data <= u16::MAX as u32
                    } else {
                        self.data == 0
                    })
                    && (self.flags == 0
                        && self.x.unsigned_abs() <= 32767
                        && self.y.unsigned_abs() <= 32767
                        || self.flags != 0 && self.x == 0 && self.y == 0),
                "invalid emulated motion"
            );
        } else if self.kind == 1 {
            ensure!(
                (8..=254).contains(&self.data)
                    && self.flags <= 7
                    && self.flags & 6 != 6
                    && self.y == 0
                    && (1..=255).contains(&self.x),
                "invalid emulated key"
            );
        } else {
            ensure!(
                self.flags == 0 && self.x == 0 && self.y == 0 && (1..=65535).contains(&self.data),
                "invalid UTF-16 text packet"
            );
        }
        Ok(())
    }
}

pub fn text_packets(device: u32, text: &str) -> Result<Vec<Packet>> {
    ensure!(
        !text.is_empty() && text.encode_utf16().count() <= 4096,
        "Text must contain 1–4096 UTF-16 units"
    );
    ensure!(
        text.chars()
            .all(|c| !c.is_control() || matches!(c, '\n' | '\t')),
        "Text contains an unsupported control character"
    );
    let packets: Vec<_> = text
        .encode_utf16()
        .map(|unit| Packet::text_unit(device, unit))
        .collect();
    for packet in &packets {
        packet.validate()?;
    }
    Ok(packets)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn identities_and_packets_are_distinct_and_checked() {
        let mouse = device(DeviceKind::Mouse, 1).unwrap();
        let keyboard = device(DeviceKind::Keyboard, 1).unwrap();
        assert_ne!(mouse.handle, keyboard.handle);
        assert_eq!(parse(&mouse.path).unwrap().unwrap().handle, mouse.handle);
        assert!(parse("virtual://mouse/01").is_err());
        assert!(device(DeviceKind::Mouse, 0).is_err());
        assert!(
            Packet::motion(mouse.handle as u32, 80, -20)
                .validate()
                .is_ok()
        );
        assert!(
            Packet::motion(keyboard.handle as u32, 80, -20)
                .validate()
                .is_err()
        );
        assert!(
            Packet::key(keyboard.handle as u32, 0x57, false)
                .validate()
                .is_ok()
        );
        assert!(
            Packet::key(keyboard.handle as u32, 0xFF, false)
                .validate()
                .is_err()
        );
        for button in 0..5 {
            for released in [false, true] {
                assert!(
                    Packet::button(mouse.handle as u32, button, released)
                        .validate()
                        .is_ok()
                );
            }
        }
        assert!(
            Packet::button(mouse.handle as u32, 5, false)
                .validate()
                .is_err()
        );
        // Held-state routing uses one transition identity per packet. Combined
        // down/up flags must be split by the producer before entering routing.
        for flags in [3, 5, 64 | 256, 2 | 8] {
            assert!(
                Packet {
                    flags,
                    ..Packet::motion(mouse.handle as u32, 0, 0)
                }
                .validate()
                .is_err()
            );
        }
        for delta in [i16::MIN, -120, 120, i16::MAX] {
            assert!(Packet::wheel(mouse.handle as u32, delta).validate().is_ok());
            assert!(
                Packet::horizontal_wheel(mouse.handle as u32, delta)
                    .validate()
                    .is_ok()
            );
        }
        assert_eq!(std::mem::size_of::<Packet>(), 24);
    }
}
