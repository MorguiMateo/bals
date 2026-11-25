use std::collections::VecDeque;
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU32, Ordering},
};
use std::thread::{self, JoinHandle};

use tracing::debug;
use wasapi::{DeviceEnumerator, Direction, SampleType, StreamMode, WaveFormat};

/// Medidor de volumen (VU Meter) en tiempo real para la ventana de Ajustes.
///
/// Captura silenciosamente los picos de audio del escritorio (loopback) y del micrófono
/// usando WASAPI y expone sus niveles (`0.0..=1.0`) de forma *lock-free* para la UI.
/// Al cerrar la ventana (`Drop`), ambos hilos terminan de inmediato (0% CPU/RAM en juego).
pub struct AudioLevelMonitor {
    desktop_peak: Arc<AtomicU32>,
    mic_peak: Arc<AtomicU32>,
    stop: Arc<AtomicBool>,
    desktop_handle: Option<JoinHandle<()>>,
    mic_handle: Option<JoinHandle<()>>,
}

impl AudioLevelMonitor {
    pub fn start() -> Self {
        let desktop_peak = Arc::new(AtomicU32::new(0.0f32.to_bits()));
        let mic_peak = Arc::new(AtomicU32::new(0.0f32.to_bits()));
        let stop = Arc::new(AtomicBool::new(false));

        let d_peak = Arc::clone(&desktop_peak);
        let d_stop = Arc::clone(&stop);
        let desktop_handle = thread::Builder::new()
            .name("bals-vu-desktop".to_string())
            .spawn(move || monitor_loop(Direction::Render, d_peak, d_stop))
            .ok();

        let m_peak = Arc::clone(&mic_peak);
        let m_stop = Arc::clone(&stop);
        let mic_handle = thread::Builder::new()
            .name("bals-vu-mic".to_string())
            .spawn(move || monitor_loop(Direction::Capture, m_peak, m_stop))
            .ok();

        Self {
            desktop_peak,
            mic_peak,
            stop,
            desktop_handle,
            mic_handle,
        }
    }

    /// Nivel instantáneo suavizado del audio de escritorio/juego (`0.0..=1.0`).
    pub fn desktop_level(&self) -> f32 {
        f32::from_bits(self.desktop_peak.load(Ordering::Relaxed))
    }

    /// Nivel instantáneo suavizado del micrófono (`0.0..=1.0`).
    pub fn mic_level(&self) -> f32 {
        f32::from_bits(self.mic_peak.load(Ordering::Relaxed))
    }
}

impl Drop for AudioLevelMonitor {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(h) = self.desktop_handle.take() {
            let _ = h.join();
        }
        if let Some(h) = self.mic_handle.take() {
            let _ = h.join();
        }
    }
}

fn monitor_loop(device_dir: Direction, level_out: Arc<AtomicU32>, stop: Arc<AtomicBool>) {
    let _ = wasapi::initialize_mta();

    let Ok(enumerator) = DeviceEnumerator::new() else {
        return;
    };
    let Ok(device) = enumerator.get_default_device(device_dir) else {
        return;
    };
    let Ok(mut client) = device.get_iaudioclient() else {
        return;
    };

    let format = WaveFormat::new(32, 32, SampleType::Float, 48_000, 2, None);
    let mode = StreamMode::EventsShared {
        autoconvert: true,
        buffer_duration_hns: 200_000,
    };
    // En Render + Direction::Capture activa loopback del sistema; en Capture abre el micrófono.
    if client
        .initialize_client(&format, Direction::Capture, mode, None)
        .is_err()
    {
        return;
    }

    let Ok(h_event) = client.set_get_eventhandle() else {
        return;
    };
    let Ok(capture) = client.get_audiocaptureclient() else {
        return;
    };
    if client.start_stream().is_err() {
        return;
    }

    let mut raw: VecDeque<u8> = VecDeque::with_capacity(8192);
    let mut smoothed = 0.0f32;

    while !stop.load(Ordering::Relaxed) {
        let _ = h_event.wait_for_event(30);
        if stop.load(Ordering::Relaxed) {
            break;
        }

        let mut chunk_peak = 0.0f32;
        loop {
            let frames = match capture.get_next_packet_size() {
                Ok(Some(n)) => n,
                _ => 0,
            };
            if frames == 0 {
                break;
            }
            if capture.read_from_device_to_deque(&mut raw).is_err() {
                break;
            }
            while raw.len() >= 4 {
                let b0 = raw.pop_front().unwrap();
                let b1 = raw.pop_front().unwrap();
                let b2 = raw.pop_front().unwrap();
                let b3 = raw.pop_front().unwrap();
                let v = f32::from_le_bytes([b0, b1, b2, b3]).abs();
                if v > chunk_peak {
                    chunk_peak = v;
                }
            }
        }

        // Ataque instantáneo y caída suave tipo VU meter profesional.
        if chunk_peak > smoothed {
            smoothed = chunk_peak;
        } else {
            smoothed *= 0.78;
            if smoothed < 0.002 {
                smoothed = 0.0;
            }
        }
        level_out.store(smoothed.clamp(0.0, 1.5).to_bits(), Ordering::Relaxed);
    }

    let _ = client.stop_stream();
    debug!(?device_dir, "Monitor VU detenido");
}
