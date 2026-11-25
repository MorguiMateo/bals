use anyhow::{Result, bail};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    HOT_KEY_MODIFIERS, MOD_ALT, MOD_CONTROL, MOD_NOREPEAT, MOD_SHIFT, MOD_WIN, VK_OEM_5,
    VkKeyScanW,
};

/// Definición Win32 lista para pasar a `RegisterHotKey`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParsedHotkey {
    pub modifiers: HOT_KEY_MODIFIERS,
    pub vk: u32,
}

/// Parsea una cadena legible (ej. `"\\"`, `"F8"`, `"Alt+F10"`, `"Ctrl+Shift+S"`)
/// en `(HOT_KEY_MODIFIERS, virtual_key)` para `RegisterHotKey`.
pub fn parse_hotkey(spec: &str) -> Result<ParsedHotkey> {
    let trimmed = spec.trim();
    if trimmed.is_empty() {
        bail!("El atajo de teclado está vacío");
    }

    let mut modifiers = MOD_NOREPEAT;
    let mut key_token: Option<&str> = None;

    for raw_part in trimmed.split('+') {
        let part = raw_part.trim();
        if part.is_empty() {
            continue;
        }
        match part.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => modifiers |= MOD_CONTROL,
            "alt" => modifiers |= MOD_ALT,
            "shift" => modifiers |= MOD_SHIFT,
            "win" | "super" => modifiers |= MOD_WIN,
            _ => key_token = Some(part),
        }
    }

    let key = key_token.ok_or_else(|| anyhow::anyhow!("Falta la tecla principal en '{spec}'"))?;
    let vk = resolve_virtual_key(key)?;

    Ok(ParsedHotkey { modifiers, vk })
}

fn resolve_virtual_key(key: &str) -> Result<u32> {
    // Tecla por defecto elegida para Bals: barra invertida `\`
    // Si el teclado activo tiene `\` sin modificadores (layout US), usa ese VK exacto;
    // en teclados ES/LatAm donde `\` requiere AltGr, vinculamos directamente `VK_OEM_5`
    // (la tecla física de `\`/`º`/`|` a la izquierda del 1) para disparo de 1 solo toque en juego.
    if key == "\\" || key.eq_ignore_ascii_case("backslash") {
        let scan = unsafe { VkKeyScanW('\\' as u16) };
        if scan != -1 {
            let vk_low = (scan & 0xFF) as u32;
            let shift_state = (scan >> 8) & 0xFF;
            if shift_state == 0 && vk_low != 0 {
                return Ok(vk_low);
            }
        }
        return Ok(VK_OEM_5.0 as u32);
    }

    let upper = key.to_ascii_uppercase();

    // Teclas de función F1..F24
    if let Some(num_str) = upper.strip_prefix('F') {
        if let Ok(n) = num_str.parse::<u32>() {
            if (1..=24).contains(&n) {
                return Ok(0x70 + (n - 1)); // VK_F1 = 0x70
            }
        }
    }

    // Letras A-Z y dígitos 0-9
    if upper.len() == 1 {
        let b = upper.as_bytes()[0];
        if b.is_ascii_alphanumeric() {
            return Ok(b as u32);
        }
    }

    // Teclas especiales frecuentes
    match upper.as_str() {
        "INSERT" | "INS" => Ok(0x2D),
        "HOME" => Ok(0x24),
        "END" => Ok(0x23),
        "PGUP" | "PAGEUP" => Ok(0x21),
        "PGDN" | "PAGEDOWN" => Ok(0x22),
        "PAUSE" => Ok(0x13),
        "SCROLLLOCK" => Ok(0x91),
        "NUMPAD0" => Ok(0x60),
        "NUMPAD1" => Ok(0x61),
        "NUMPAD2" => Ok(0x62),
        "NUMPAD3" => Ok(0x63),
        "NUMPAD4" => Ok(0x64),
        "NUMPAD5" => Ok(0x65),
        "NUMPAD6" => Ok(0x66),
        "NUMPAD7" => Ok(0x67),
        "NUMPAD8" => Ok(0x68),
        "NUMPAD9" => Ok(0x69),
        "/" | "SLASH" => Ok(0xBF),     // VK_OEM_2
        "-" | "MINUS" => Ok(0xBD),     // VK_OEM_MINUS
        "=" | "EQUALS" => Ok(0xBB),    // VK_OEM_PLUS
        "[" | "LBRACKET" => Ok(0xDB),  // VK_OEM_4
        "]" | "RBRACKET" => Ok(0xDD),  // VK_OEM_6
        "`" | "TILDE" => Ok(0xC0),     // VK_OEM_3
        _ => bail!("Tecla no reconocida para hotkey: '{key}'"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_default_backslash_and_combos() {
        let bs = parse_hotkey("\\").unwrap();
        assert_eq!(bs.modifiers, MOD_NOREPEAT);
        assert_ne!(bs.vk, 0);

        let f8 = parse_hotkey("F8").unwrap();
        assert_eq!(f8.vk, 0x77);
        assert_eq!(f8.modifiers, MOD_NOREPEAT);

        let combo = parse_hotkey("Alt+F10").unwrap();
        assert_eq!(combo.vk, 0x79);
        assert_eq!(combo.modifiers, MOD_NOREPEAT | MOD_ALT);

        let combo2 = parse_hotkey("Ctrl+Shift+S").unwrap();
        assert_eq!(combo2.vk, b'S' as u32);
        assert_eq!(combo2.modifiers, MOD_NOREPEAT | MOD_CONTROL | MOD_SHIFT);
    }
}
