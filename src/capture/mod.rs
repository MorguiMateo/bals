pub mod wgc;

use windows::Graphics::Capture::Direct3D11CaptureFrame;
use windows::Win32::Foundation::{HWND, LPARAM, RECT};
use windows::Win32::Graphics::Direct3D11::ID3D11Texture2D;
use windows::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO, MONITORINFOEXW,
};
use windows::core::BOOL;

const MONITORINFOF_PRIMARY_FLAG: u32 = 1;

/// Objetivo de captura (monitor principal, monitor específico o ventana de un juego).
#[derive(Debug, Clone, Copy)]
#[allow(dead_code)]
pub enum CaptureTarget {
    PrimaryMonitor,
    Monitor(HMONITOR),
    Window(HWND),
}

/// Información de un monitor conectado enumerado vía Win32 (`EnumDisplayMonitors`).
#[derive(Debug, Clone)]
pub struct MonitorInfo {
    /// Índice amigable base 1 (`1`, `2`, ...) con el monitor principal siempre primero.
    pub index: usize,
    /// Nombre estable del dispositivo en Windows (ej. `\\.\DISPLAY1`).
    pub device_name: String,
    pub width: u32,
    pub height: u32,
    pub left: i32,
    pub top: i32,
    pub is_primary: bool,
    pub hmonitor: HMONITOR,
}

unsafe impl Send for MonitorInfo {}

impl MonitorInfo {
    /// Etiqueta corta para mostrar en el LCD/bandeja (ej. `PANTALLA 1`).
    pub fn short_label(&self) -> String {
        format!("PANTALLA {}", self.index)
    }

    /// Etiqueta descriptiva para botones de la UI (ej. `PANTALLA 1 · 1920x1080 ★`).
    pub fn ui_label(&self) -> String {
        if self.is_primary {
            format!("PANTALLA {} · {}x{} ★", self.index, self.width, self.height)
        } else {
            format!("PANTALLA {} · {}x{}", self.index, self.width, self.height)
        }
    }
}

/// Enumera todos los monitores activos en Windows (0 dependencias externas).
/// Ordena dejando el monitor principal primero y luego de izquierda a derecha.
pub fn list_monitors() -> Vec<MonitorInfo> {
    let mut monitors: Vec<MonitorInfo> = Vec::new();
    let ptr = (&mut monitors as *mut Vec<MonitorInfo>) as isize;
    unsafe {
        let _ = EnumDisplayMonitors(None, None, Some(enum_monitor_proc), LPARAM(ptr));
    }

    monitors.sort_by(|a, b| {
        b.is_primary
            .cmp(&a.is_primary)
            .then_with(|| a.left.cmp(&b.left))
            .then_with(|| a.top.cmp(&b.top))
            .then_with(|| a.device_name.cmp(&b.device_name))
    });

    for (i, m) in monitors.iter_mut().enumerate() {
        m.index = i + 1;
    }

    monitors
}

unsafe extern "system" fn enum_monitor_proc(
    hmonitor: HMONITOR,
    _hdc: HDC,
    _clip_rect: *mut RECT,
    data: LPARAM,
) -> BOOL {
    let monitors = unsafe { &mut *(data.0 as *mut Vec<MonitorInfo>) };

    let mut info = MONITORINFOEXW::default();
    info.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
    let ok = unsafe {
        GetMonitorInfoW(hmonitor, (&mut info as *mut MONITORINFOEXW).cast::<MONITORINFO>())
    }
    .as_bool();

    if ok {
        let rc = info.monitorInfo.rcMonitor;
        let width = rc.right.saturating_sub(rc.left).unsigned_abs();
        let height = rc.bottom.saturating_sub(rc.top).unsigned_abs();
        let nul_pos = info
            .szDevice
            .iter()
            .position(|&c| c == 0)
            .unwrap_or(info.szDevice.len());
        let device_name = String::from_utf16_lossy(&info.szDevice[..nul_pos]);
        let is_primary = (info.monitorInfo.dwFlags & MONITORINFOF_PRIMARY_FLAG) != 0;

        monitors.push(MonitorInfo {
            index: 0,
            device_name,
            width,
            height,
            left: rc.left,
            top: rc.top,
            is_primary,
            hmonitor,
        });
    }

    BOOL(1)
}

/// Resuelve el `CaptureTarget` y la etiqueta corta para el monitor configurado.
/// Si el monitor elegido fue desconectado (o es `None`), cae limpiamente al monitor principal.
pub fn resolve_monitor_target(selected_device: Option<&str>) -> (CaptureTarget, String) {
    let monitors = list_monitors();
    if let Some(wanted) = selected_device.map(str::trim).filter(|s| !s.is_empty()) {
        if let Some(m) = monitors
            .iter()
            .find(|m| m.device_name.eq_ignore_ascii_case(wanted))
        {
            return (CaptureTarget::Monitor(m.hmonitor), m.short_label());
        }
    }

    if let Some(primary) = monitors.first() {
        return (CaptureTarget::Monitor(primary.hmonitor), primary.short_label());
    }

    (CaptureTarget::PrimaryMonitor, "MONITOR".to_string())
}

/// Frame capturado en GPU (Zero-Copy).
///
/// Mantiene vivo `_wgc_frame` para que el FramePool de WGC no reutilice la textura
/// hasta que el consumidor (VideoProcessor / Encoder) termine de usarla y haga `drop`.
pub struct CapturedFrame {
    pub texture: ID3D11Texture2D,
    pub width: u32,
    pub height: u32,
    /// Marca de tiempo relativa del sistema en unidades de 100 nanosegundos (ticks de WinRT/MF).
    #[allow(dead_code)]
    pub timestamp_100ns: i64,
    _wgc_frame: Direct3D11CaptureFrame,
}

// Los objetos COM de D3D11 con ID3D11Multithread activo y Direct3D11CaptureFrame
// de un FramePool FreeThreaded pueden transferirse entre hilos de forma segura.
unsafe impl Send for CapturedFrame {}

impl CapturedFrame {
    pub(crate) fn new(
        texture: ID3D11Texture2D,
        width: u32,
        height: u32,
        timestamp_100ns: i64,
        wgc_frame: Direct3D11CaptureFrame,
    ) -> Self {
        Self {
            texture,
            width,
            height,
            timestamp_100ns,
            _wgc_frame: wgc_frame,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn monitor_labels_and_fallback_target() {
        let primary = MonitorInfo {
            index: 1,
            device_name: r"\\.\DISPLAY1".to_string(),
            width: 1920,
            height: 1080,
            left: 0,
            top: 0,
            is_primary: true,
            hmonitor: HMONITOR::default(),
        };
        assert_eq!(primary.short_label(), "PANTALLA 1");
        assert_eq!(primary.ui_label(), "PANTALLA 1 · 1920x1080 ★");

        let secondary = MonitorInfo {
            index: 2,
            device_name: r"\\.\DISPLAY2".to_string(),
            width: 2560,
            height: 1440,
            left: 1920,
            top: 0,
            is_primary: false,
            hmonitor: HMONITOR::default(),
        };
        assert_eq!(secondary.ui_label(), "PANTALLA 2 · 2560x1440");

        let (_target, label) = resolve_monitor_target(Some(r"\\.\DISPLAY_NON_EXISTENT"));
        assert!(!label.is_empty());
    }
}
