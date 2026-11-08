//! Virtual key name parsing and read-only layout-to-scan translation; never OS input injection.
use anyhow::{Result, bail, ensure};
use mux_core::emulated::Packet;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyboardLayout, MAPVK_VK_TO_VSC_EX, MapVirtualKeyExW,
};

pub fn code(name: &str) -> Result<u32> {
    let normalized = name
        .trim()
        .to_ascii_lowercase()
        .replace(['-', '_', ' '], "");
    if normalized.len() == 1 {
        let b = normalized.as_bytes()[0];
        if b.is_ascii_alphanumeric() {
            return Ok(u32::from(b.to_ascii_uppercase()));
        }
    }
    if let Some(number) = normalized
        .strip_prefix('f')
        .and_then(|n| n.parse::<u32>().ok())
    {
        ensure!((1..=24).contains(&number), "Function key must be F1–F24");
        return Ok(0x6F + number);
    }
    if let Some(number) = normalized
        .strip_prefix("numpad")
        .and_then(|n| n.parse::<u32>().ok())
    {
        ensure!(number <= 9, "Numpad key must be 0–9");
        return Ok(0x60 + number);
    }
    Ok(match normalized.as_str() {
        "backspace" => 8,
        "tab" => 9,
        "enter" | "return" => 13,
        "shift" => 16,
        "ctrl" | "control" => 17,
        "alt" => 18,
        "pause" => 19,
        "capslock" => 20,
        "escape" | "esc" => 27,
        "space" => 32,
        "pageup" => 33,
        "pagedown" => 34,
        "end" => 35,
        "home" => 36,
        "left" => 37,
        "up" => 38,
        "right" => 39,
        "down" => 40,
        "printscreen" => 44,
        "insert" => 45,
        "delete" | "del" => 46,
        "leftwin" => 91,
        "rightwin" => 92,
        "menu" => 93,
        "multiply" => 106,
        "add" => 107,
        "subtract" => 109,
        "decimal" => 110,
        "divide" => 111,
        "numlock" => 144,
        "scrolllock" => 145,
        "leftshift" => 160,
        "rightshift" => 161,
        "leftctrl" => 162,
        "rightctrl" => 163,
        "leftalt" => 164,
        "rightalt" => 165,
        "semicolon" | ";" => 186,
        "equals" | "=" => 187,
        "comma" | "," => 188,
        "minus" => 189,
        "period" | "." => 190,
        "slash" | "/" => 191,
        "backtick" | "`" => 192,
        "leftbracket" | "[" => 219,
        "backslash" | "\\" => 220,
        "rightbracket" | "]" => 221,
        "quote" | "'" => 222,
        _ => bail!(
            "Unknown key '{name}'; use letters, digits, F1–F24, modifiers, navigation or named punctuation"
        ),
    })
}
pub fn packet(device: u32, name: &str, released: bool) -> Result<Packet> {
    packet_code(device, code(name)?, released)
}
pub fn packet_code(device: u32, key: u32, released: bool) -> Result<Packet> {
    ensure!((8..=254).contains(&key), "Virtual-key code is out of range");
    let scan = unsafe { MapVirtualKeyExW(key, MAPVK_VK_TO_VSC_EX, GetKeyboardLayout(0)) };
    ensure!(
        scan & 0xFF != 0,
        "Key has no scan code in the current keyboard layout"
    );
    // Some layouts return the legacy keypad scan for dedicated navigation keys,
    // even with VK_TO_VSC_EX. Preserve their physical E0 identity explicitly;
    // numpad digit keys deliberately remain unextended.
    let extended = match scan >> 8 {
        0xE0 => 2,
        0xE1 => 4,
        _ if matches!(key, 0x21..=0x28 | 0x2C..=0x2E | 0x5B..=0x5D | 0x6F | 0x90 | 0xA3 | 0xA5) => {
            2
        }
        _ => 0,
    };
    let packet = Packet::key_scanned(device, key, (scan & 0xFF) as i32, extended, released);
    packet.validate()?;
    Ok(packet)
}
