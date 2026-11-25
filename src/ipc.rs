use std::process::Command;

use tracing::warn;
use windows::Win32::Foundation::{LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{
    FindWindowW, PostMessageW, SW_RESTORE, SetForegroundWindow, ShowWindow, WM_APP,
};
use windows::core::{PCWSTR, w};

pub const TRAY_WINDOW_CLASS: PCWSTR = w!("BalsTrayWindowClass");
pub const UI_WINDOW_TITLE: PCWSTR = w!("Bals");

/// Mensajes IPC ultraligeros (sin sockets ni pipes) entre la ventana `--ui` y el daemon en bandeja.
pub const WM_BALS_RELOAD_CONFIG: u32 = WM_APP + 10;
pub const WM_BALS_TOGGLE_RECORD: u32 = WM_APP + 11;
pub const WM_BALS_SAVE_CLIP: u32 = WM_APP + 12;
pub const WM_BALS_SHUTDOWN: u32 = WM_APP + 13;

/// Devuelve `true` si ya existe una instancia del daemon de Bals corriendo en la bandeja.
pub fn is_daemon_running() -> bool {
    unsafe { FindWindowW(TRAY_WINDOW_CLASS, PCWSTR::null()) }
        .map(|h| !h.is_invalid())
        .unwrap_or(false)
}

/// Avisa al daemon en segundo plano que recargue `%APPDATA%\Bals\config.toml`.
pub fn notify_reload_config() -> bool {
    post_to_daemon(WM_BALS_RELOAD_CONFIG)
}

/// Solicita al daemon iniciar o pausar el Replay Buffer manual.
pub fn notify_toggle_record() -> bool {
    post_to_daemon(WM_BALS_TOGGLE_RECORD)
}

/// Solicita al daemon guardar inmediatamente los últimos N segundos del buffer.
pub fn notify_save_clip() -> bool {
    post_to_daemon(WM_BALS_SAVE_CLIP)
}

/// Solicita al daemon cerrarse ordenadamente (usado al aplicar una actualización automática).
pub fn notify_shutdown() -> bool {
    post_to_daemon(WM_BALS_SHUTDOWN)
}

fn post_to_daemon(msg: u32) -> bool {
    match unsafe { FindWindowW(TRAY_WINDOW_CLASS, PCWSTR::null()) } {
        Ok(hwnd) if !hwnd.is_invalid() => {
            unsafe { PostMessageW(Some(hwnd), msg, WPARAM(0), LPARAM(0)) }.is_ok()
        }
        _ => false,
    }
}

/// Abre la ventana nativa liviana (`bals.exe --ui`) como proceso independiente de corta vida.
/// Si ya está abierta, la trae al frente; al cerrarla, el SO libera el 100% de su RAM y contexto GPU.
pub fn spawn_or_focus_ui() {
    if let Ok(hwnd) = unsafe { FindWindowW(PCWSTR::null(), UI_WINDOW_TITLE) } {
        if !hwnd.is_invalid() {
            unsafe {
                let _ = ShowWindow(hwnd, SW_RESTORE);
                let _ = SetForegroundWindow(hwnd);
            }
            return;
        }
    }

    match std::env::current_exe() {
        Ok(exe) => {
            if let Err(err) = Command::new(exe).arg("--ui").spawn() {
                warn!("No se pudo lanzar la ventana UI de Bals: {err}");
            }
        }
        Err(err) => warn!("No se pudo resolver current_exe: {err}"),
    }
}
