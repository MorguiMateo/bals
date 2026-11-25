pub mod wgc;

use windows::Graphics::Capture::Direct3D11CaptureFrame;
use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Direct3D11::ID3D11Texture2D;
use windows::Win32::Graphics::Gdi::HMONITOR;

/// Objetivo de captura (monitor principal, monitor específico o ventana de un juego).
#[derive(Debug, Clone, Copy)]
#[allow(dead_code)]
pub enum CaptureTarget {
    PrimaryMonitor,
    Monitor(HMONITOR),
    Window(HWND),
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
