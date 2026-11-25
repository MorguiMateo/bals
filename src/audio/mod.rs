pub mod aac_mft;
pub mod capture;
pub mod meter;

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use crossbeam_channel::{Receiver, TrySendError, bounded};
use tracing::{debug, info, warn};

use crate::audio::aac_mft::AacEncoder;
use crate::audio::capture::{AUDIO_CHANNELS, AUDIO_SAMPLE_RATE, CaptureKind, WasapiStream};
use crate::clock;
use crate::config::AudioConfig;

/// Paquete AAC-LC codificado (payload raw) con marca de tiempo en el mismo reloj QPC que el video.
#[derive(Debug, Clone)]
pub struct EncodedAudioPacket {
    pub data: Vec<u8>,
    /// Timestamp absoluto de presentación en ticks de 100 ns (`clock::now_100ns()`).
    pub pts_100ns: i64,
    /// Duración del paquete en ticks de 100 ns (~213_333 para 1024 muestras @ 48 kHz).
    pub duration_100ns: i64,
}

/// Metadatos del flujo AAC necesarios para que el muxer MP4 escriba la pista de audio en passthrough.
#[derive(Debug, Clone)]
pub struct AudioStreamInfo {
    pub sample_rate: u32,
    pub channels: u16,
    pub bitrate_bps: u32,
    /// `MF_MT_USER_DATA` (`HEAACWAVEINFO` + `AudioSpecificConfig`) generado por el AAC MFT.
    pub user_data: Vec<u8>,
}

/// Pipeline completo de audio:
/// Captura juego/sistema (WASAPI Loopback) + micrófono opcional → mezcla PCM 48 kHz con
/// soft-clipping → codifica AAC-LC vía Media Foundation → emite paquetes sincronizados.
pub struct AudioPipeline {
    pub rx: Receiver<EncodedAudioPacket>,
    pub stream_info: AudioStreamInfo,
    stop: Arc<AtomicBool>,
    mixer_handle: Option<JoinHandle<()>>,
    _game_stream: WasapiStream,
    _mic_stream: Option<WasapiStream>,
}

impl AudioPipeline {
    pub fn start(config: AudioConfig) -> Result<Self> {
        let game_stream = WasapiStream::start(CaptureKind::GameOrSystem(config.source))
            .context("No se pudo iniciar la captura de audio del juego/sistema")?;

        let mic_stream = if config.mic_enabled {
            match WasapiStream::start(CaptureKind::Microphone) {
                Ok(mic) => Some(mic),
                Err(err) => {
                    warn!("Micrófono no disponible ({err:#}); grabando solo audio del juego/sistema");
                    None
                }
            }
        } else {
            None
        };

        let game_queue = Arc::clone(&game_stream.samples);
        let mic_queue = mic_stream.as_ref().map(|m| Arc::clone(&m.samples));

        let (tx, rx) = bounded::<EncodedAudioPacket>(256);
        let (info_tx, info_rx) = crossbeam_channel::bounded::<Result<AudioStreamInfo>>(1);
        let stop = Arc::new(AtomicBool::new(false));
        let stop_clone = Arc::clone(&stop);

        let mixer_handle = thread::Builder::new()
            .name("bals-audio-mixer-aac".to_string())
            .spawn(move || {
                mixer_aac_thread(config, game_queue, mic_queue, tx, info_tx, stop_clone);
            })
            .context("No se pudo iniciar el hilo mezclador/encoder AAC")?;

        let stream_info = info_rx
            .recv()
            .context("El hilo encoder AAC terminó antes de inicializar")??;

        info!(
            mic_active = mic_stream.is_some(),
            bitrate_kbps = stream_info.bitrate_bps / 1000,
            "Pipeline de audio de Bals activo"
        );

        Ok(Self {
            rx,
            stream_info,
            stop,
            mixer_handle: Some(mixer_handle),
            _game_stream: game_stream,
            _mic_stream: mic_stream,
        })
    }

    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(handle) = self.mixer_handle.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for AudioPipeline {
    fn drop(&mut self) {
        self.stop();
    }
}

fn mixer_aac_thread(
    config: AudioConfig,
    game_queue: Arc<std::sync::Mutex<std::collections::VecDeque<f32>>>,
    mic_queue: Option<Arc<std::sync::Mutex<std::collections::VecDeque<f32>>>>,
    tx: crossbeam_channel::Sender<EncodedAudioPacket>,
    info_tx: crossbeam_channel::Sender<Result<AudioStreamInfo>>,
    stop: Arc<AtomicBool>,
) {
    let mut encoder = match AacEncoder::new(config) {
        Ok(enc) => {
            let info = AudioStreamInfo {
                sample_rate: enc.sample_rate,
                channels: enc.channels,
                bitrate_bps: enc.bitrate_bps,
                user_data: enc.user_data.clone(),
            };
            let _ = info_tx.send(Ok(info));
            enc
        }
        Err(err) => {
            let _ = info_tx.send(Err(err));
            return;
        }
    };

    // Un bloque AAC-LC exacto = 1024 frames estéreo = 2048 muestras f32 = 21.333 ms @ 48 kHz.
    const FRAMES_PER_CHUNK: usize = 1024;
    const SAMPLES_PER_CHUNK: usize = FRAMES_PER_CHUNK * AUDIO_CHANNELS;
    let chunk_dur_100ns = (FRAMES_PER_CHUNK as i64 * 10_000_000) / AUDIO_SAMPLE_RATE as i64;

    let mut game_buf = vec![0.0f32; SAMPLES_PER_CHUNK];
    let mut mic_buf = vec![0.0f32; SAMPLES_PER_CHUNK];
    let mut pcm_i16 = vec![0i16; SAMPLES_PER_CHUNK];

    // Reloj continuo anclado a `clock::now_100ns()` al inicio, avanzando exactamente `chunk_dur_100ns`
    // por cada bloque de 1024 muestras (evita huecos/solapamientos de timestamps en MP4).
    let mut next_pts_100ns = clock::now_100ns();
    let mut next_wakeup = Instant::now() + Duration::from_nanos((chunk_dur_100ns * 100) as u64);

    while !stop.load(Ordering::Relaxed) {
        let now = Instant::now();
        if next_wakeup > now {
            let sleep_dur = (next_wakeup - now).min(Duration::from_millis(10));
            thread::sleep(sleep_dur);
            continue;
        }

        // Extraer 1024 frames del juego/sistema (rellenando silencio 0.0 si el sistema está mudo).
        drain_or_silence(&game_queue, &mut game_buf);

        // Extraer 1024 frames del micrófono (si está habilitado).
        if let Some(mic_q) = &mic_queue {
            drain_or_silence(mic_q, &mut mic_buf);
        } else {
            mic_buf.fill(0.0);
        }

        // Mezcla en punto flotante con soft-clipping `tanh` para evitar saturación brusca.
        let g_vol = config.game_volume;
        let m_vol = config.mic_volume;
        for i in 0..SAMPLES_PER_CHUNK {
            let mixed = game_buf[i] * g_vol + mic_buf[i] * m_vol;
            let limited = if mixed.abs() > 0.95 {
                mixed.tanh()
            } else {
                mixed
            };
            pcm_i16[i] = (limited.clamp(-1.0, 1.0) * i16::MAX as f32) as i16;
        }

        match encoder.encode_pcm_i16(&pcm_i16, next_pts_100ns) {
            Ok(packets) => {
                for pkt in packets {
                    if let Err(TrySendError::Disconnected(_)) = tx.try_send(pkt) {
                        return;
                    }
                }
            }
            Err(err) => {
                warn!("Error codificando bloque AAC: {err:#}");
            }
        }

        next_pts_100ns += chunk_dur_100ns;
        next_wakeup += Duration::from_nanos((chunk_dur_100ns * 100) as u64);

        // Si el hilo se atrasó mucho (>150 ms, ej. hibernación), resincronizar el reloj.
        let wall_now = Instant::now();
        if wall_now > next_wakeup + Duration::from_millis(150) {
            debug!("Resincronizando reloj del mezclador de audio tras pausa del sistema");
            next_pts_100ns = clock::now_100ns();
            next_wakeup = wall_now + Duration::from_nanos((chunk_dur_100ns * 100) as u64);
        }
    }

    if let Ok(packets) = encoder.flush() {
        for pkt in packets {
            let _ = tx.try_send(pkt);
        }
    }
}

fn drain_or_silence(
    queue: &std::sync::Mutex<std::collections::VecDeque<f32>>,
    out: &mut [f32],
) {
    let mut guard = queue.lock().unwrap();
    let available = guard.len().min(out.len());
    for sample in out.iter_mut().take(available) {
        *sample = guard.pop_front().unwrap();
    }
    if available < out.len() {
        out[available..].fill(0.0);
    }
}
