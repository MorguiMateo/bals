use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};

use tracing::warn;
use windows::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE, LPARAM, WPARAM};
use windows::Win32::System::Memory::{
    CreateFileMappingW, FILE_MAP_ALL_ACCESS, FILE_MAP_READ, MEMORY_MAPPED_VIEW_ADDRESS,
    MapViewOfFile, OpenFileMappingW, PAGE_READWRITE, UnmapViewOfFile,
};
use windows::Win32::UI::WindowsAndMessaging::{
    FindWindowW, PostMessageW, SW_RESTORE, SetForegroundWindow, ShowWindow, WM_APP,
};
use windows::core::{PCWSTR, w};

pub const TRAY_WINDOW_CLASS: PCWSTR = w!("BalsTrayWindowClass");
pub const UI_WINDOW_TITLE: PCWSTR = w!("Bals");
const TELEMETRY_MAP_NAME: PCWSTR = w!("Local\\BalsEngineTelemetryV1");
const TELEMETRY_MAGIC: u32 = 0x4241_4C53; // "BALS"

/// Mensajes IPC ultraligeros (sin sockets ni pipes) entre la ventana `--ui` y el daemon en bandeja.
pub const WM_BALS_RELOAD_CONFIG: u32 = WM_APP + 10;
pub const WM_BALS_TOGGLE_RECORD: u32 = WM_APP + 11;
pub const WM_BALS_SAVE_CLIP: u32 = WM_APP + 12;
pub const WM_BALS_SHUTDOWN: u32 = WM_APP + 13;

/// Estado compacto de grabación publicado por memoria compartida (0 I/O en disco).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TelemetryState {
    Idle,
    RecordingManual,
    RecordingGame,
    PausedAltTab,
}

impl TelemetryState {
    fn from_u32(v: u32) -> Self {
        match v {
            1 => Self::RecordingManual,
            2 => Self::RecordingGame,
            3 => Self::PausedAltTab,
            _ => Self::Idle,
        }
    }

    pub fn to_u32(self) -> u32 {
        match self {
            Self::Idle => 0,
            Self::RecordingManual => 1,
            Self::RecordingGame => 2,
            Self::PausedAltTab => 3,
        }
    }
}

/// Instantánea legible en la UI de la telemetría viva del motor en segundo plano.
#[derive(Debug, Clone)]
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

#[repr(C)]
struct RawSharedTelemetry {
    magic: u32,
    /// Seqlock ligero: impar mientras el daemon escribe, par cuando es consistente.
    seq: AtomicU32,
    state_code: u32,
    replay_target_secs: u32,
    buffered_secs: f32,
    buffer_ram_mb: f32,
    clip_event_seq: u32,
    clip_event_is_err: u32,
    game_name_len: u32,
    game_name: [u8; 64],
    encoder_len: u32,
    encoder_label: [u8; 40],
    last_event_len: u32,
    last_event: [u8; 128],
}

/// Escritor de telemetría en memoria compartida propiedad del hilo `bals-engine`.
pub struct TelemetryWriter {
    handle: HANDLE,
    view: MEMORY_MAPPED_VIEW_ADDRESS,
    clip_event_seq: u32,
    clip_event_is_err: bool,
    last_event: String,
}

unsafe impl Send for TelemetryWriter {}

impl TelemetryWriter {
    pub fn create() -> Option<Self> {
        let size = std::mem::size_of::<RawSharedTelemetry>() as u32;
        let handle = unsafe {
            CreateFileMappingW(
                INVALID_HANDLE_VALUE,
                None,
                PAGE_READWRITE,
                0,
                size,
                TELEMETRY_MAP_NAME,
            )
        }
        .ok()?;
        if handle.is_invalid() {
            return None;
        }

        let view = unsafe { MapViewOfFile(handle, FILE_MAP_ALL_ACCESS, 0, 0, size as usize) };
        if view.Value.is_null() {
            unsafe {
                let _ = CloseHandle(handle);
            }
            return None;
        }

        // Inicializar bloque a cero y sellar magic.
        unsafe {
            std::ptr::write_bytes(view.Value as *mut u8, 0, size as usize);
            let raw = &mut *(view.Value as *mut RawSharedTelemetry);
            raw.magic = TELEMETRY_MAGIC;
            raw.seq.store(0, Ordering::Release);
        }

        Some(Self {
            handle,
            view,
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
        let raw = unsafe { &mut *(self.view.Value as *mut RawSharedTelemetry) };
        let start_seq = raw.seq.load(Ordering::Relaxed);
        let writing_seq = (start_seq & !1).wrapping_add(1);
        raw.seq.store(writing_seq, Ordering::Release);

        raw.magic = TELEMETRY_MAGIC;
        raw.state_code = state.to_u32();
        raw.replay_target_secs = replay_target_secs;
        raw.buffered_secs = buffered_secs;
        raw.buffer_ram_mb = buffer_ram_mb;
        raw.clip_event_seq = self.clip_event_seq;
        raw.clip_event_is_err = u32::from(self.clip_event_is_err);
        raw.game_name_len = write_utf8_bytes(&mut raw.game_name, game_name);
        raw.encoder_len = write_utf8_bytes(&mut raw.encoder_label, encoder_label);
        raw.last_event_len = write_utf8_bytes(&mut raw.last_event, &self.last_event);

        raw.seq.store(writing_seq.wrapping_add(1), Ordering::Release);
    }
}

impl Drop for TelemetryWriter {
    fn drop(&mut self) {
        unsafe {
            let _ = UnmapViewOfFile(self.view);
            let _ = CloseHandle(self.handle);
        }
    }
}

/// Lee de forma instantánea y sin I/O en disco la telemetría publicada por el daemon.
pub fn read_engine_telemetry() -> Option<EngineTelemetrySnapshot> {
    let size = std::mem::size_of::<RawSharedTelemetry>();
    let handle = unsafe { OpenFileMappingW(FILE_MAP_READ.0, false, TELEMETRY_MAP_NAME) }.ok()?;
    if handle.is_invalid() {
        return None;
    }

    let view = unsafe { MapViewOfFile(handle, FILE_MAP_READ, 0, 0, size) };
    if view.Value.is_null() {
        unsafe {
            let _ = CloseHandle(handle);
        }
        return None;
    }

    let mut result = None;
    let raw = unsafe { &*(view.Value as *const RawSharedTelemetry) };

    for _ in 0..4 {
        let seq1 = raw.seq.load(Ordering::Acquire);
        if seq1 & 1 != 0 {
            std::hint::spin_loop();
            continue;
        }
        if raw.magic != TELEMETRY_MAGIC {
            break;
        }

        let state = TelemetryState::from_u32(raw.state_code);
        let replay_target_secs = raw.replay_target_secs;
        let buffered_secs = raw.buffered_secs;
        let buffer_ram_mb = raw.buffer_ram_mb;
        let clip_event_seq = raw.clip_event_seq;
        let clip_event_is_err = raw.clip_event_is_err != 0;
        let game_name = read_utf8_bytes(&raw.game_name, raw.game_name_len);
        let encoder_label = read_utf8_bytes(&raw.encoder_label, raw.encoder_len);
        let last_event = read_utf8_bytes(&raw.last_event, raw.last_event_len);

        std::sync::atomic::fence(Ordering::Acquire);
        let seq2 = raw.seq.load(Ordering::Acquire);
        if seq1 == seq2 {
            result = Some(EngineTelemetrySnapshot {
                state,
                replay_target_secs,
                buffered_secs,
                buffer_ram_mb,
                clip_event_seq,
                clip_event_is_err,
                game_name,
                encoder_label,
                last_event,
            });
            break;
        }
    }

    unsafe {
        let _ = UnmapViewOfFile(view);
        let _ = CloseHandle(handle);
    }

    result
}

fn write_utf8_bytes(dst: &mut [u8], text: &str) -> u32 {
    let bytes = text.as_bytes();
    let mut len = bytes.len().min(dst.len());
    while len > 0 && !text.is_char_boundary(len) {
        len -= 1;
    }
    dst[..len].copy_from_slice(&bytes[..len]);
    if len < dst.len() {
        dst[len..].fill(0);
    }
    len as u32
}

fn read_utf8_bytes(src: &[u8], len: u32) -> String {
    let clamped = (len as usize).min(src.len());
    String::from_utf8_lossy(&src[..clamped]).into_owned()
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
