use std::path::Path;

use windows::Win32::Foundation::{CloseHandle, HWND, RECT, STILL_ACTIVE};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromWindow,
};
use windows::Win32::System::Threading::{
    GetExitCodeProcess, OpenProcess, PROCESS_NAME_FORMAT, PROCESS_QUERY_LIMITED_INFORMATION,
    QueryFullProcessImageNameW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetForegroundWindow, GetWindowRect, GetWindowThreadProcessId, IsIconic, IsWindowVisible,
};
use windows::core::PWSTR;

/// Procesos comunes de escritorio, navegadores o herramientas que NO deben activar
/// la grabación automática aunque se pongan en pantalla completa.
const IGNORED_EXES: &[&str] = &[
    "bals.exe",
    "explorer.exe",
    "searchhost.exe",
    "shellexperiencehost.exe",
    "startmenuexperiencehost.exe",
    "applicationframehost.exe",
    "lockapp.exe",
    "dwm.exe",
    "taskmgr.exe",
    "chrome.exe",
    "msedge.exe",
    "firefox.exe",
    "brave.exe",
    "opera.exe",
    "vivaldi.exe",
    "discord.exe",
    "spotify.exe",
    "code.exe",
    "devenv.exe",
    "windowsterminal.exe",
    "cmd.exe",
    "powershell.exe",
    "pwsh.exe",
    "vlc.exe",
    "obs64.exe",
];

#[derive(Debug, Clone)]
pub struct DetectedGame {
    pub hwnd: HWND,
    pub pid: u32,
    pub exe_name: String,
}

// `HWND` es un handle numérico del SO; puede transferirse de forma segura.
unsafe impl Send for DetectedGame {}

/// Inspecciona la ventana en primer plano con impacto casi nulo en CPU (~0.01%).
///
/// Considera que hay un juego activo si:
/// 1. El ejecutable figura explícitamente en `extra_game_exes` (incluso en modo ventana), o
/// 2. Ocupa pantalla completa / borderless (>=98% del monitor) y no está en la lista de apps de escritorio.
pub fn detect_foreground_game(extra_game_exes: &[String]) -> Option<DetectedGame> {
    let hwnd = unsafe { GetForegroundWindow() };
    if hwnd.is_invalid() {
        return None;
    }

    unsafe {
        if !IsWindowVisible(hwnd).as_bool() || IsIconic(hwnd).as_bool() {
            return None;
        }
    }

    let mut pid = 0u32;
    unsafe {
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
    }
    if pid == 0 || pid == std::process::id() {
        return None;
    }

    let exe_name = process_exe_name(pid)?;
    let exe_lower = exe_name.to_ascii_lowercase();

    // 1. Coincidencia explícita en la lista del usuario (funciona también en modo ventana).
    if extra_game_exes
        .iter()
        .any(|g| g.trim().eq_ignore_ascii_case(&exe_lower))
    {
        return Some(DetectedGame {
            hwnd,
            pid,
            exe_name,
        });
    }

    // 2. Descartar apps de sistema/navegadores conocidos.
    if IGNORED_EXES.iter().any(|&ignored| ignored == exe_lower) {
        return None;
    }

    // 3. Heurística liviana: ventana a pantalla completa o borderless fullscreen.
    if is_fullscreen_window(hwnd) {
        return Some(DetectedGame {
            hwnd,
            pid,
            exe_name,
        });
    }

    None
}

/// Comprueba si un proceso sigue ejecutándose (evita perder el buffer al hacer Alt-Tab breve).
pub fn is_process_alive(pid: u32) -> bool {
    let Ok(handle) = (unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }) else {
        return false;
    };
    let mut exit_code = 0u32;
    let ok = unsafe { GetExitCodeProcess(handle, &mut exit_code) }.is_ok();
    unsafe {
        let _ = CloseHandle(handle);
    }
    ok && exit_code == STILL_ACTIVE.0 as u32
}

fn process_exe_name(pid: u32) -> Option<String> {
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()?;
    let mut buf = [0u16; 512];
    let mut size = buf.len() as u32;
    let res = unsafe {
        QueryFullProcessImageNameW(
            handle,
            PROCESS_NAME_FORMAT(0),
            PWSTR(buf.as_mut_ptr()),
            &mut size,
        )
    };
    unsafe {
        let _ = CloseHandle(handle);
    }
    res.ok()?;

    let full_path = String::from_utf16_lossy(&buf[..size as usize]);
    Path::new(&full_path)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
}

fn is_fullscreen_window(hwnd: HWND) -> bool {
    let mut win_rect = RECT::default();
    if unsafe { GetWindowRect(hwnd, &mut win_rect) }.is_err() {
        return false;
    }

    let win_w = win_rect.right - win_rect.left;
    let win_h = win_rect.bottom - win_rect.top;
    if win_w < 640 || win_h < 480 {
        return false;
    }

    let monitor = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) };
    let mut mi = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    if !unsafe { GetMonitorInfoW(monitor, &mut mi) }.as_bool() {
        return false;
    }

    let mon_w = mi.rcMonitor.right - mi.rcMonitor.left;
    let mon_h = mi.rcMonitor.bottom - mi.rcMonitor.top;
    if mon_w <= 0 || mon_h <= 0 {
        return false;
    }

    // Cubre >= 98% del ancho y alto del monitor (pantalla completa exclusiva o borderless).
    (win_w * 100 >= mon_w * 98) && (win_h * 100 >= mon_h * 98)
}
