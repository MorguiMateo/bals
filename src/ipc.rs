use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tracing::warn;
use windows::Win32::Foundation::{LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{
    FindWindowW, PostMessageW, SW_RESTORE, SetForegroundWindow, ShowWindow, WM_APP, WM_CLOSE,
};
use windows::core::{PCWSTR, w};

pub const TRAY_WINDOW_CLASS: PCWSTR = w!("BalsTrayWindowClass");
pub const UI_WINDOW_TITLE: PCWSTR = w!("Bals");

/// Mensajes IPC ultraligeros (sin sockets ni memoria compartida Win32) entre `--ui` y el daemon.
pub const WM_BALS_RELOAD_CONFIG: u32 = WM_APP + 10;
pub const WM_BALS_TOGGLE_RECORD: u32 = WM_APP + 11;
pub const WM_BALS_SAVE_CLIP: u32 = WM_APP + 12;
pub const WM_BALS_SHUTDOWN: u32 = WM_APP + 13;
pub const WM_BALS_CHECK_UPDATE: u32 = WM_APP + 14;
pub const WM_BALS_PAUSE_HOTKEY: u32 = WM_APP + 15;
pub const WM_BALS_RESUME_HOTKEY: u32 = WM_APP + 16;

/// Estado compacto de grabación publicado por el daemon para el HUD de la UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TelemetryState {
    Idle,
    RecordingManual,
    RecordingGame,
    PausedAltTab,
}

/// Instantánea legible en la UI de la telemetría viva del motor en segundo plano.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EngineTelemetrySnapshot {
    pub state: TelemetryState,
    pub replay_target_secs: u32,
    pub buffered_secs: f32,
    pub buffer_ram_mb: f32,
    pub clip_event_seq: u32,
    pub clip_event_is_err: bool,
    pub game_name: String,
    pub encoder_label: String,
    pub last_event: String,
}

fn telemetry_file_path() -> PathBuf {
    std::env::temp_dir().join("bals_engine_telemetry.toml")
}

/// Escritor de telemetría liviano basado en archivo temporal en `%TEMP%`.
/// Deduplica escrituras para realizar 0 I/O mientras el motor está en espera (`Idle`).
pub struct TelemetryWriter {
    path: PathBuf,
    tmp_path: PathBuf,
    last_written: String,
    clip_event_seq: u32,
    clip_event_is_err: bool,
    last_event: String,
}

impl TelemetryWriter {
    pub fn create() -> Option<Self> {
        let path = telemetry_file_path();
        let tmp_path = std::env::temp_dir().join("bals_engine_telemetry.toml.tmp");
        Some(Self {
            path,
            tmp_path,
            last_written: String::new(),
            clip_event_seq: 0,
            clip_event_is_err: false,
            last_event: String::new(),
        })
    }

    pub fn record_clip_event(&mut self, msg: &str, is_err: bool) {
        self.clip_event_seq = self.clip_event_seq.wrapping_add(1).max(1);
        self.clip_event_is_err = is_err;
        self.last_event.clear();
        self.last_event.push_str(msg);
    }

    pub fn publish(
        &mut self,
        state: TelemetryState,
        replay_target_secs: u32,
        buffered_secs: f32,
        buffer_ram_mb: f32,
        game_name: &str,
        encoder_label: &str,
    ) {
        // Redondear a 1 decimal para evitar escrituras redundantes por ruido de flotantes.
        let snap = EngineTelemetrySnapshot {
            state,
            replay_target_secs,
            buffered_secs: (buffered_secs * 10.0).round() / 10.0,
            buffer_ram_mb: (buffer_ram_mb * 10.0).round() / 10.0,
            clip_event_seq: self.clip_event_seq,
            clip_event_is_err: self.clip_event_is_err,
            game_name: game_name.to_string(),
            encoder_label: encoder_label.to_string(),
            last_event: self.last_event.clone(),
        };

        let Ok(serialized) = toml::to_string(&snap) else {
            return;
        };
        if serialized == self.last_written {
            return;
        }

        if std::fs::write(&self.tmp_path, &serialized).is_ok() {
            if std::fs::rename(&self.tmp_path, &self.path).is_err() {
                let _ = std::fs::write(&self.path, &serialized);
                let _ = std::fs::remove_file(&self.tmp_path);
            }
            self.last_written = serialized;
        }
    }
}

impl Drop for TelemetryWriter {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.tmp_path);
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Lee la telemetría viva publicada por el daemon.
pub fn read_engine_telemetry() -> Option<EngineTelemetrySnapshot> {
    let raw = std::fs::read_to_string(telemetry_file_path()).ok()?;
    toml::from_str(&raw).ok()
}

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

/// Solicita al daemon comprobar si hay una actualización disponible (al abrir la app).
pub fn notify_check_update() -> bool {
    post_to_daemon(WM_BALS_CHECK_UPDATE)
}

/// Pausa temporalmente la hotkey global mientras el usuario está asignando una tecla en la UI.
pub fn notify_pause_hotkey() -> bool {
    post_to_daemon(WM_BALS_PAUSE_HOTKEY)
}

/// Reanuda la hotkey global al terminar o cancelar la asignación de tecla en la UI.
pub fn notify_resume_hotkey() -> bool {
    post_to_daemon(WM_BALS_RESUME_HOTKEY)
}

/// Espera hasta que el daemon en segundo plano cierre su ventana de bandeja (o venza `timeout`).
pub fn wait_until_daemon_stopped(timeout: Duration) -> bool {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if !is_daemon_running() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    !is_daemon_running()
}

/// Devuelve `true` si la ventana de interfaz (`bals.exe --ui`) está abierta actualmente.
pub fn is_ui_open() -> bool {
    let tray_hwnd = unsafe { FindWindowW(TRAY_WINDOW_CLASS, PCWSTR::null()) }.ok();
    if let Ok(hwnd) = unsafe { FindWindowW(PCWSTR::null(), UI_WINDOW_TITLE) } {
        return !hwnd.is_invalid() && Some(hwnd) != tray_hwnd;
    }
    false
}

/// Cierra ordenadamente la ventana de escritorio (`bals.exe --ui`) si está abierta.
pub fn close_ui_window() {
    let tray_hwnd = unsafe { FindWindowW(TRAY_WINDOW_CLASS, PCWSTR::null()) }.ok();
    if let Ok(hwnd) = unsafe { FindWindowW(PCWSTR::null(), UI_WINDOW_TITLE) } {
        if !hwnd.is_invalid() && Some(hwnd) != tray_hwnd {
            unsafe {
                let _ = PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
            }
        }
    }
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
    let tray_hwnd = unsafe { FindWindowW(TRAY_WINDOW_CLASS, PCWSTR::null()) }.ok();

    if let Ok(hwnd) = unsafe { FindWindowW(PCWSTR::null(), UI_WINDOW_TITLE) } {
        if !hwnd.is_invalid() && Some(hwnd) != tray_hwnd {
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
