use anyhow::{Context, Result};
use windows::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_QUERY_VALUE, KEY_SET_VALUE, REG_SZ, RegCloseKey, RegDeleteValueW,
    RegOpenKeyExW, RegQueryValueExW, RegSetValueExW,
};
use windows::core::{PCWSTR, w};

const RUN_SUBKEY: PCWSTR = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run");
const VALUE_NAME: PCWSTR = w!("Bals");

/// Comprueba si Bals está configurado para iniciarse automáticamente con Windows (`HKCU\...\Run`).
pub fn is_autostart_enabled() -> bool {
    let mut hkey = HKEY::default();
    let open = unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, RUN_SUBKEY, Some(0), KEY_QUERY_VALUE, &mut hkey) };
    if open.is_err() {
        return false;
    }

    let mut data_len = 0u32;
    let res = unsafe {
        RegQueryValueExW(hkey, VALUE_NAME, None, None, None, Some(&mut data_len))
    };
    unsafe {
        let _ = RegCloseKey(hkey);
    }
    res.is_ok() && data_len > 0
}

/// Habilita o deshabilita el inicio automático con Windows (sin permisos de administrador).
pub fn set_autostart_enabled(enabled: bool) -> Result<()> {
    let mut hkey = HKEY::default();
    unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, RUN_SUBKEY, Some(0), KEY_SET_VALUE, &mut hkey) }
        .ok()
        .context("No se pudo abrir la clave de inicio de Windows en el registro")?;

    let result = if enabled {
        let exe = std::env::current_exe().context("No se pudo resolver current_exe")?;
        let cmd = format!("\"{}\" --tray", exe.display());
        let wide: Vec<u16> = cmd.encode_utf16().chain(std::iter::once(0)).collect();
        let bytes = unsafe {
            std::slice::from_raw_parts(wide.as_ptr() as *const u8, wide.len() * 2)
        };
        unsafe { RegSetValueExW(hkey, VALUE_NAME, Some(0), REG_SZ, Some(bytes)) }
            .ok()
            .context("RegSetValueExW falló al activar inicio automático")
    } else {
        // Ignorar si ya no existía la entrada.
        let _ = unsafe { RegDeleteValueW(hkey, VALUE_NAME) };
        Ok(())
    };

    unsafe {
        let _ = RegCloseKey(hkey);
    }
    result
}
