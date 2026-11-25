use std::collections::BTreeMap;
use std::path::Path;

use windows::Win32::Foundation::{CloseHandle, HWND, LPARAM, RECT, STILL_ACTIVE};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, HMONITOR, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromWindow,
};
use windows::Win32::System::Threading::{
    GetExitCodeProcess, OpenProcess, PROCESS_NAME_FORMAT, PROCESS_QUERY_LIMITED_INFORMATION,
    QueryFullProcessImageNameW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetForegroundWindow, GetWindowRect, GetWindowTextLengthW, GetWindowTextW,
    GetWindowThreadProcessId, IsIconic, IsWindowVisible,
};
use windows::core::{BOOL, PWSTR};

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
    /// Nombre limpio y amigable del juego para carpetas y prefijos de clips (ej. `"Valorant"`).
    pub game_name: String,
    pub is_fullscreen: bool,
    pub monitor: HMONITOR,
}

// `HWND` y `HMONITOR` son handles numéricos del SO; pueden transferirse de forma segura.
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
    let monitor = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) };
    let win_title = window_title_text(hwnd);
    let game_name = resolve_game_name(&exe_name, &win_title);

    // 1. Coincidencia explícita en la lista del usuario (funciona también en modo ventana).
    if extra_game_exes
        .iter()
        .any(|g| g.trim().eq_ignore_ascii_case(&exe_lower))
    {
        return Some(DetectedGame {
            hwnd,
            pid,
            exe_name,
            game_name,
            is_fullscreen: is_fullscreen_window(hwnd),
            monitor,
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
            game_name,
            is_fullscreen: true,
            monitor,
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

/// Procesos internos del shell de Windows (y el propio Bals) que se ocultan del selector de `.exe`.
const PICKER_SHELL_EXES: &[&str] = &[
    "bals.exe",
    "explorer.exe",
    "searchhost.exe",
    "shellexperiencehost.exe",
    "startmenuexperiencehost.exe",
    "textinputhost.exe",
    "applicationframehost.exe",
    "lockapp.exe",
    "dwm.exe",
    "systemsettings.exe",
    "taskmgr.exe",
    "nvidia overlay.exe",
    "nvcontainer.exe",
];

/// Aplicación activa con ventana visible detectada en el sistema (para selector estilo vibranceGUI).
#[derive(Debug, Clone)]
pub struct RunningApp {
    pub exe_name: String,
    pub window_title: String,
}

/// Enumera las aplicaciones activas con ventanas visibles en el escritorio, deduplicadas por `.exe`.
pub fn list_running_window_apps() -> Vec<RunningApp> {
    let mut found: BTreeMap<String, RunningApp> = BTreeMap::new();
    let ptr = &mut found as *mut BTreeMap<String, RunningApp> as isize;
    unsafe {
        let _ = EnumWindows(Some(enum_visible_windows_proc), LPARAM(ptr));
    }
    found.into_values().collect()
}

unsafe extern "system" fn enum_visible_windows_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
    unsafe {
        if !IsWindowVisible(hwnd).as_bool() {
            return BOOL(1);
        }
        let title_len = GetWindowTextLengthW(hwnd);
        if title_len <= 0 {
            return BOOL(1);
        }

        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if pid == 0 || pid == std::process::id() {
            return BOOL(1);
        }

        let Some(exe_name) = process_exe_name(pid) else {
            return BOOL(1);
        };
        let exe_lower = exe_name.to_ascii_lowercase();
        if PICKER_SHELL_EXES.iter().any(|&sh| sh == exe_lower) {
            return BOOL(1);
        }

        let mut buf = vec![0u16; (title_len as usize) + 1];
        let copied = GetWindowTextW(hwnd, &mut buf);
        if copied <= 0 {
            return BOOL(1);
        }
        let title = String::from_utf16_lossy(&buf[..copied as usize])
            .trim()
            .to_string();
        if title.is_empty() {
            return BOOL(1);
        }

        let map = &mut *(lparam.0 as *mut BTreeMap<String, RunningApp>);
        map.entry(exe_lower)
            .and_modify(|existing| {
                if title.len() > existing.window_title.len() {
                    existing.window_title = title.clone();
                }
            })
            .or_insert(RunningApp {
                exe_name,
                window_title: title,
            });

        BOOL(1)
    }
}

fn window_title_text(hwnd: HWND) -> String {
    unsafe {
        let len = GetWindowTextLengthW(hwnd);
        if len <= 0 {
            return String::new();
        }
        let mut buf = vec![0u16; (len as usize) + 1];
        let copied = GetWindowTextW(hwnd, &mut buf);
        if copied <= 0 {
            return String::new();
        }
        String::from_utf16_lossy(&buf[..copied as usize])
            .trim()
            .to_string()
    }
}

/// Convierte el nombre del `.exe` (y opcionalmente el título de ventana) en un nombre de juego
/// limpio y estable para crear carpetas y prefijos de clip (ej. `"VALORANT-Win64-Shipping.exe"` → `"Valorant"`).
pub fn resolve_game_name(exe_name: &str, window_title: &str) -> String {
    let lower = exe_name.trim().to_ascii_lowercase();
    let title_lower = window_title.to_ascii_lowercase();

    let known = match lower.as_str() {
        "valorant-win64-shipping.exe" | "valorant.exe" => Some("Valorant"),
        "cs2.exe" => Some("Counter-Strike 2"),
        "csgo.exe" => Some("CSGO"),
        "league of legends.exe" => Some("League of Legends"),
        "fortniteclient-win64-shipping.exe" | "fortnite.exe" => Some("Fortnite"),
        "r5apex.exe" | "r5apex_dx12.exe" => Some("Apex Legends"),
        "overwatch.exe" => Some("Overwatch"),
        "rocketleague.exe" => Some("Rocket League"),
        "gta5.exe" | "gta5_enhanced.exe" => Some("GTA V"),
        "cod.exe" | "modernwarfare.exe" => Some("Call of Duty"),
        "tslgame.exe" => Some("PUBG"),
        "rainbowsix.exe" | "rainbowsix_vulkan.exe" | "rainbowsix_be.exe" => {
            Some("Rainbow Six Siege")
        }
        "dota2.exe" => Some("Dota 2"),
        "minecraft.windows.exe" => Some("Minecraft"),
        "javaw.exe" | "java.exe" if title_lower.contains("minecraft") => Some("Minecraft"),
        "eldenring.exe" => Some("Elden Ring"),
        "cyberpunk2077.exe" => Some("Cyberpunk 2077"),
        "deadbydaylight-win64-shipping.exe" => Some("Dead by Daylight"),
        "marvel-win64-shipping.exe" => Some("Marvel Rivals"),
        "escapefromtarkov.exe" => Some("Escape from Tarkov"),
        "rustclient.exe" => Some("Rust"),
        "fc24.exe" | "fc25.exe" | "fc26.exe" => Some("EA Sports FC"),
        "genshinimpact.exe" => Some("Genshin Impact"),
        "osu!.exe" => Some("osu"),
        _ => None,
    };

    if let Some(name) = known {
        return name.to_string();
    }

    // Quitar extensión `.exe` y sufijos técnicos habituales de Unreal/Unity/64-bit.
    let mut stem = exe_name
        .trim()
        .strip_suffix(".exe")
        .or_else(|| exe_name.trim().strip_suffix(".EXE"))
        .unwrap_or(exe_name.trim())
        .to_string();

    for suffix in [
        "-Win64-Shipping",
        "-win64-shipping",
        "-Win64-Test",
        "-Shipping",
        "_Win64",
        "_x64",
        "-x64",
        "Client",
    ] {
        if stem.len() > suffix.len() {
            if let Some(stripped) = stem.strip_suffix(suffix) {
                stem = stripped.to_string();
                break;
            }
        }
    }

    let cleaned = stem.replace(['_', '-'], " ");
    let words: Vec<String> = cleaned
        .split_whitespace()
        .map(|w| {
            let all_same_case =
                w.chars().all(|c| !c.is_alphabetic() || c.is_lowercase())
                    || w.chars().all(|c| !c.is_alphabetic() || c.is_uppercase());
            if all_same_case && w.len() > 2 {
                let mut chars = w.chars();
                match chars.next() {
                    Some(first) => {
                        format!(
                            "{}{}",
                            first.to_uppercase(),
                            chars.as_str().to_lowercase()
                        )
                    }
                    None => String::new(),
                }
            } else {
                w.to_string()
            }
        })
        .collect();

    let joined = words.join(" ");
    if joined.is_empty() {
        "Game".to_string()
    } else {
        joined
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_friendly_game_names() {
        assert_eq!(resolve_game_name("VALORANT-Win64-Shipping.exe", "VALORANT"), "Valorant");
        assert_eq!(resolve_game_name("cs2.exe", "Counter-Strike 2"), "Counter-Strike 2");
        assert_eq!(resolve_game_name("javaw.exe", "Minecraft 1.21.4"), "Minecraft");
        assert_eq!(resolve_game_name("palworld-win64-shipping.exe", "Palworld"), "Palworld");
        assert_eq!(resolve_game_name("HollowKnight.exe", "Hollow Knight"), "HollowKnight");
    }
}
