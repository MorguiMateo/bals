use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU64, Ordering},
};

use anyhow::{Context, Result};
use crossbeam_channel::{Receiver, TrySendError, bounded};
use tracing::{debug, info, warn};
use windows::Foundation::TypedEventHandler;
use windows::Graphics::Capture::{
    Direct3D11CaptureFramePool, GraphicsCaptureItem, GraphicsCaptureSession,
};
use windows::Graphics::DirectX::DirectXPixelFormat;
use windows::Graphics::SizeInt32;
use windows::Win32::Foundation::{HWND, POINT};
use windows::Win32::Graphics::Direct3D11::ID3D11Texture2D;
use windows::Win32::Graphics::Gdi::{HMONITOR, MONITOR_DEFAULTTOPRIMARY, MonitorFromPoint};
use windows::Win32::System::WinRT::Direct3D11::IDirect3DDxgiInterfaceAccess;
use windows::Win32::System::WinRT::Graphics::Capture::IGraphicsCaptureItemInterop;
use windows::core::{Interface, factory};

use crate::capture::{CaptureTarget, CapturedFrame};
use crate::d3d::SharedD3DDevice;

/// Wrapper `Send` para `IDirect3DDevice` (seguro porque inicializamos WinRT en MTA
/// y activamos `ID3D11Multithread::SetMultithreadProtected(true)` en `SharedD3DDevice`).
struct SendWinRtDevice(windows::Graphics::DirectX::Direct3D11::IDirect3DDevice);
unsafe impl Send for SendWinRtDevice {}

impl SendWinRtDevice {
    fn get(&self) -> &windows::Graphics::DirectX::Direct3D11::IDirect3DDevice {
        &self.0
    }
}

/// Estadísticas atómicas en tiempo real de la sesión de captura.
#[derive(Default)]
pub struct CaptureStats {
    pub frames_received: AtomicU64,
    pub frames_dropped: AtomicU64,
}

/// Sesión activa de Windows Graphics Capture (WGC).
/// Al hacer `drop` detiene la captura y libera el FramePool automáticamente.
pub struct WgcCapture {
    frame_pool: Direct3D11CaptureFramePool,
    session: GraphicsCaptureSession,
    _item: GraphicsCaptureItem,
    frame_arrived_token: i64,
    closed: Arc<AtomicBool>,
    pub stats: Arc<CaptureStats>,
    pub rx: Receiver<CapturedFrame>,
    pub initial_size: SizeInt32,
}

impl WgcCapture {
    /// Inicia la captura Zero-Copy usando `CreateFreeThreaded` (sin requerir DispatcherQueue).
    ///
    /// - `capture_cursor`: incluir cursor del mouse en los clips.
    /// - `channel_capacity`: tamaño máximo de la cola de frames en vuelo (recomendado: 2).
    pub fn start(
        d3d: &SharedD3DDevice,
        target: CaptureTarget,
        capture_cursor: bool,
        channel_capacity: usize,
    ) -> Result<Self> {
        let item = create_capture_item(target)?;
        let initial_size = item
            .Size()
            .context("No se pudo obtener el tamaño inicial del GraphicsCaptureItem")?;

        // Número de buffers en el FramePool = capacidad del canal + 1 para que WGC siempre
        // tenga al menos una superficie libre mientras el encoder procesa la anterior.
        let pool_buffers = (channel_capacity.max(1) + 1) as i32;

        let frame_pool = Direct3D11CaptureFramePool::CreateFreeThreaded(
            &d3d.winrt_device,
            DirectXPixelFormat::B8G8R8A8UIntNormalized,
            pool_buffers,
            initial_size,
        )
        .context("Direct3D11CaptureFramePool::CreateFreeThreaded falló")?;

        let session = frame_pool
            .CreateCaptureSession(&item)
            .context("CreateCaptureSession falló")?;

        // Desactivar el borde amarillo de captura en Windows 11+ (best-effort en Windows 10).
        if let Err(err) = session.SetIsBorderRequired(false) {
            debug!("SetIsBorderRequired(false) no soportado en esta versión de Windows: {err}");
        }
        if let Err(err) = session.SetIsCursorCaptureEnabled(capture_cursor) {
            debug!("SetIsCursorCaptureEnabled no soportado: {err}");
        }

        let (tx, rx) = bounded::<CapturedFrame>(channel_capacity.max(1));
        let stats = Arc::new(CaptureStats::default());
        let closed = Arc::new(AtomicBool::new(false));

        let last_size = Arc::new(std::sync::Mutex::new(initial_size));
        let winrt_device_clone = SendWinRtDevice(d3d.winrt_device.clone());
        let stats_clone = Arc::clone(&stats);
        let closed_clone = Arc::clone(&closed);

        // Callback invocado en un hilo del threadpool de WinRT cada vez que hay un nuevo frame.
        let handler = TypedEventHandler::<Direct3D11CaptureFramePool, windows::core::IInspectable>::new(
            move |sender, _| {
                if closed_clone.load(Ordering::Relaxed) {
                    return Ok(());
                }

                let Some(pool) = sender.as_ref() else {
                    return Ok(());
                };

                let frame = match pool.TryGetNextFrame() {
                    Ok(f) => f,
                    Err(_) => return Ok(()),
                };

                let content_size = match frame.ContentSize() {
                    Ok(s) if s.Width > 0 && s.Height > 0 => s,
                    _ => return Ok(()),
                };

                // Si la ventana del juego cambió de resolución, recreamos el FramePool al vuelo.
                {
                    let mut guard = last_size.lock().unwrap();
                    if guard.Width != content_size.Width || guard.Height != content_size.Height {
                        debug!(
                            old_w = guard.Width,
                            old_h = guard.Height,
                            new_w = content_size.Width,
                            new_h = content_size.Height,
                            "Resolución de captura modificada; recreando FramePool"
                        );
                        if let Err(err) = pool.Recreate(
                            winrt_device_clone.get(),
                            DirectXPixelFormat::B8G8R8A8UIntNormalized,
                            pool_buffers,
                            content_size,
                        ) {
                            warn!("Error recreando FramePool: {err}");
                        } else {
                            *guard = content_size;
                        }
                    }
                }

                let timestamp_100ns = frame
                    .SystemRelativeTime()
                    .map(|ts| ts.Duration)
                    .unwrap_or(0);

                // Extracción Zero-Copy de la ID3D11Texture2D directamente desde la superficie WinRT.
                let surface = match frame.Surface() {
                    Ok(s) => s,
                    Err(_) => return Ok(()),
                };
                let access: IDirect3DDxgiInterfaceAccess = match surface.cast() {
                    Ok(a) => a,
                    Err(_) => return Ok(()),
                };
                let texture: ID3D11Texture2D = match unsafe { access.GetInterface() } {
                    Ok(t) => t,
                    Err(_) => return Ok(()),
                };

                stats_clone.frames_received.fetch_add(1, Ordering::Relaxed);

                let captured = CapturedFrame::new(
                    texture,
                    content_size.Width as u32,
                    content_size.Height as u32,
                    timestamp_100ns,
                    frame,
                );

                // Backpressure no bloqueante: si el encoder va saturado, descartamos este frame
                // inmediatamente para no frenar el juego ni acumular RAM/VRAM.
                if let Err(TrySendError::Full(_)) = tx.try_send(captured) {
                    stats_clone.frames_dropped.fetch_add(1, Ordering::Relaxed);
                }

                Ok(())
            },
        );

        let frame_arrived_token = frame_pool
            .FrameArrived(&handler)
            .context("No se pudo registrar el handler FrameArrived")?;

        session
            .StartCapture()
            .context("GraphicsCaptureSession::StartCapture falló")?;

        info!(
            width = initial_size.Width,
            height = initial_size.Height,
            pool_buffers,
            "Captura WGC iniciada (Zero-Copy GPU)"
        );

        Ok(Self {
            frame_pool,
            session,
            _item: item,
            frame_arrived_token,
            closed,
            stats,
            rx,
            initial_size,
        })
    }

    pub fn stop(&self) {
        if !self.closed.swap(true, Ordering::SeqCst) {
            let _ = self.frame_pool.RemoveFrameArrived(self.frame_arrived_token);
            let _ = self.session.Close();
            let _ = self.frame_pool.Close();
        }
    }
}

impl Drop for WgcCapture {
    fn drop(&mut self) {
        self.stop();
    }
}

fn create_capture_item(target: CaptureTarget) -> Result<GraphicsCaptureItem> {
    let interop = factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>()
        .context("No se pudo obtener IGraphicsCaptureItemInterop")?;

    let item = match target {
        CaptureTarget::PrimaryMonitor => {
            let hmonitor = unsafe { MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY) };
            unsafe { interop.CreateForMonitor(hmonitor) }
                .context("CreateForMonitor falló para el monitor principal")?
        }
        CaptureTarget::Monitor(hmonitor) => unsafe { interop.CreateForMonitor(hmonitor) }
            .context("CreateForMonitor falló")?,
        CaptureTarget::Window(hwnd) => {
            if hwnd.is_invalid() {
                anyhow::bail!("HWND inválido para captura de ventana");
            }
            unsafe { interop.CreateForWindow(hwnd) }.context("CreateForWindow falló")?
        }
    };

    Ok(item)
}

/// Utilidad para obtener el HMONITOR del monitor principal.
#[allow(dead_code)]
pub fn primary_monitor() -> HMONITOR {
    unsafe { MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY) }
}

/// Utilidad para obtener la ventana activa en primer plano (útil para capturar juegos).
#[allow(dead_code)]
pub fn foreground_window() -> HWND {
    unsafe { windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow() }
}
