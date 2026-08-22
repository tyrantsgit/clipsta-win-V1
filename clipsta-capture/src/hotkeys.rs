//! Global hotkey registration using Win32 RegisterHotKey API.
//!
//! Parses hotkey strings like "Ctrl+Shift+G", "Alt+F9" into modifier+VK pairs,
//! registers them with unique IDs, and handles WM_HOTKEY in the message pump.
//!
//! Hotkey IDs:
//! - 1 = Save 30 seconds
//! - 2 = Save 1 minute
//! - 3 = Save 5 minutes

use windows::Win32::UI::Input::KeyboardAndMouse::*;

/// Hotkey IDs (matches the tray save logic).
pub const HOTKEY_ID_30S: i32 = 1;
pub const HOTKEY_ID_1MIN: i32 = 2;
pub const HOTKEY_ID_5MIN: i32 = 3;

/// Parsed hotkey: modifier flags + virtual key code.
#[derive(Debug, Clone, Copy)]
pub struct ParsedHotkey {
    pub modifiers: HOT_KEY_MODIFIERS,
    pub vk: u32,
}

/// Register all clip hotkeys. Must be called from the message pump thread.
/// Returns the number of hotkeys successfully registered.
pub fn register_all(hotkey_30s: &str, hotkey_1min: &str, hotkey_5min: &str) -> u32 {
    let mut count = 0;

    if let Some(hk) = parse_hotkey_string(hotkey_30s) {
        if register_hotkey(HOTKEY_ID_30S, hk) {
            count += 1;
        }
    }
    if let Some(hk) = parse_hotkey_string(hotkey_1min) {
        if register_hotkey(HOTKEY_ID_1MIN, hk) {
            count += 1;
        }
    }
    if let Some(hk) = parse_hotkey_string(hotkey_5min) {
        if register_hotkey(HOTKEY_ID_5MIN, hk) {
            count += 1;
        }
    }

    count
}

/// Unregister all clip hotkeys.
pub fn unregister_all() {
    unsafe {
        let _ = UnregisterHotKey(None, HOTKEY_ID_30S);
        let _ = UnregisterHotKey(None, HOTKEY_ID_1MIN);
        let _ = UnregisterHotKey(None, HOTKEY_ID_5MIN);
    }
}

/// Register a single hotkey. Returns true on success.
fn register_hotkey(id: i32, hk: ParsedHotkey) -> bool {
    unsafe {
        RegisterHotKey(None, id, hk.modifiers, hk.vk).is_ok()
    }
}

/// Parse a hotkey string like "Ctrl+Shift+G" or "Alt+F9" into modifier+VK.
///
/// Supported modifiers: Ctrl, Alt, Shift, Win/Super
/// Supported keys: A-Z, 0-9, F1-F24, plus special keys
///
/// Returns None if the string is empty or unparseable.
pub fn parse_hotkey_string(s: &str) -> Option<ParsedHotkey> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }

    let parts: Vec<&str> = s.split('+').map(|p| p.trim()).collect();
    if parts.is_empty() {
        return None;
    }

    let mut modifiers = HOT_KEY_MODIFIERS(0);
    let mut vk: Option<u32> = None;

    for part in &parts {
        let lower = part.to_lowercase();
        match lower.as_str() {
            "ctrl" | "control" => modifiers |= MOD_CONTROL,
            "alt" => modifiers |= MOD_ALT,
            "shift" => modifiers |= MOD_SHIFT,
            "win" | "super" | "meta" => modifiers |= MOD_WIN,
            _ => {
                // This should be the key itself
                vk = Some(string_to_vk(part)?);
            }
        }
    }

    let vk = vk?;
    // Add MOD_NOREPEAT to avoid repeated hotkey messages when key is held
    modifiers |= MOD_NOREPEAT;

    Some(ParsedHotkey { modifiers, vk })
}

/// Convert a key name string to a virtual key code.
fn string_to_vk(key: &str) -> Option<u32> {
    let upper = key.to_uppercase();

    // Function keys F1-F24
    if upper.starts_with('F') && upper.len() >= 2 {
        if let Ok(n) = upper[1..].parse::<u32>() {
            if (1..=24).contains(&n) {
                return Some(VK_F1.0 as u32 + (n - 1));
            }
        }
    }

    // Single character A-Z
    if upper.len() == 1 {
        let ch = upper.chars().next()?;
        if ch.is_ascii_uppercase() {
            return Some(ch as u32); // VK for A-Z = 0x41-0x5A = ASCII value
        }
        if ch.is_ascii_digit() {
            return Some(ch as u32); // VK for 0-9 = 0x30-0x39 = ASCII value
        }
    }

    // Named special keys
    match upper.as_str() {
        "SPACE" | "SPACEBAR" => Some(VK_SPACE.0 as u32),
        "ENTER" | "RETURN" => Some(VK_RETURN.0 as u32),
        "TAB" => Some(VK_TAB.0 as u32),
        "ESCAPE" | "ESC" => Some(VK_ESCAPE.0 as u32),
        "BACKSPACE" | "BACK" => Some(VK_BACK.0 as u32),
        "DELETE" | "DEL" => Some(VK_DELETE.0 as u32),
        "INSERT" | "INS" => Some(VK_INSERT.0 as u32),
        "HOME" => Some(VK_HOME.0 as u32),
        "END" => Some(VK_END.0 as u32),
        "PAGEUP" | "PGUP" => Some(VK_PRIOR.0 as u32),
        "PAGEDOWN" | "PGDN" => Some(VK_NEXT.0 as u32),
        "UP" => Some(VK_UP.0 as u32),
        "DOWN" => Some(VK_DOWN.0 as u32),
        "LEFT" => Some(VK_LEFT.0 as u32),
        "RIGHT" => Some(VK_RIGHT.0 as u32),
        "PRINTSCREEN" | "PRTSC" => Some(VK_SNAPSHOT.0 as u32),
        "SCROLLLOCK" => Some(VK_SCROLL.0 as u32),
        "PAUSE" | "BREAK" => Some(VK_PAUSE.0 as u32),
        "NUMLOCK" => Some(VK_NUMLOCK.0 as u32),
        "CAPSLOCK" => Some(VK_CAPITAL.0 as u32),
        // Numpad
        "NUM0" | "NUMPAD0" => Some(VK_NUMPAD0.0 as u32),
        "NUM1" | "NUMPAD1" => Some(VK_NUMPAD1.0 as u32),
        "NUM2" | "NUMPAD2" => Some(VK_NUMPAD2.0 as u32),
        "NUM3" | "NUMPAD3" => Some(VK_NUMPAD3.0 as u32),
        "NUM4" | "NUMPAD4" => Some(VK_NUMPAD4.0 as u32),
        "NUM5" | "NUMPAD5" => Some(VK_NUMPAD5.0 as u32),
        "NUM6" | "NUMPAD6" => Some(VK_NUMPAD6.0 as u32),
        "NUM7" | "NUMPAD7" => Some(VK_NUMPAD7.0 as u32),
        "NUM8" | "NUMPAD8" => Some(VK_NUMPAD8.0 as u32),
        "NUM9" | "NUMPAD9" => Some(VK_NUMPAD9.0 as u32),
        // OEM keys
        ";" | "SEMICOLON" => Some(VK_OEM_1.0 as u32),
        "=" | "EQUALS" | "PLUS" => Some(VK_OEM_PLUS.0 as u32),
        "," | "COMMA" => Some(VK_OEM_COMMA.0 as u32),
        "-" | "MINUS" => Some(VK_OEM_MINUS.0 as u32),
        "." | "PERIOD" => Some(VK_OEM_PERIOD.0 as u32),
        "/" | "SLASH" => Some(VK_OEM_2.0 as u32),
        "`" | "BACKTICK" | "GRAVE" => Some(VK_OEM_3.0 as u32),
        "[" | "LBRACKET" => Some(VK_OEM_4.0 as u32),
        "\\" | "BACKSLASH" => Some(VK_OEM_5.0 as u32),
        "]" | "RBRACKET" => Some(VK_OEM_6.0 as u32),
        "'" | "QUOTE" => Some(VK_OEM_7.0 as u32),
        _ => None,
    }
}
