use std::sync::OnceLock;

use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};

fn qpc_frequency() -> i64 {
    static FREQ: OnceLock<i64> = OnceLock::new();
    *FREQ.get_or_init(|| {
        let mut f = 0i64;
        unsafe {
            let _ = QueryPerformanceFrequency(&mut f);
        }
        f.max(1)
    })
}

/// Devuelve el reloj monotónico de alta precisión de Windows en ticks de 100 nanosegundos.
///
/// Usa exactamente la misma base temporal (`QueryPerformanceCounter`) que
/// `Direct3D11CaptureFrame::SystemRelativeTime()` de WGC, permitiendo sincronizar
/// video y audio con precisión de microsegundos sin deriva.
pub fn now_100ns() -> i64 {
    let mut counter = 0i64;
    unsafe {
        let _ = QueryPerformanceCounter(&mut counter);
    }
    let freq = qpc_frequency() as i128;
    ((counter as i128 * 10_000_000) / freq) as i64
}
