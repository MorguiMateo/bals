use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use crossbeam_channel::{Receiver, Sender, unbounded};
use tracing::{error, info, warn};
use windows::Win32::Graphics::Direct3D11::ID3D11Texture2D;
use windows::Win32::Media::{timeBeginPeriod, timeEndPeriod};
use windows::Win32::System::WinRT::{RO_INIT_MULTITHREADED, RoInitialize, RoUninitialize};

use crate::audio::AudioPipeline;
use crate::buffer::ReplayBuffer;
use crate::capture::CaptureTarget;
use crate::capture::wgc::WgcCapture;
use crate::clock;
use crate::config::{AppConfig, AudioSource, CaptureMode, VideoConfig};
use crate::d3d::SharedD3DDevice;
use crate::encode::h264_mft::H264Encoder;
use crate::encode::video_processor::GpuNv12Converter;
use crate::game_detect::{DetectedGame, detect_foreground_game, is_process_alive};
use crate::muxer::{WriteStats, generate_clip_path, write_mp4_clip};

/// Comandos que la bandeja del sistema, hotkeys o GUI envían al motor de grabación.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub enum EngineCommand {
    StartManual,
    Stop,
    ToggleManual,
    SaveClip,
    UpdateConfig(AppConfig),
    Shutdown,
}

/// Eventos que el motor emite hacia la bandeja del sistema / GUI (para notificaciones y UI).
#[derive(Debug, Clone)]
pub enum EngineEvent {
    ClipSaved(WriteStats),
    ClipFailed(String),
    StatusChanged(EngineStatus),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecordingState {
    Idle,
    RecordingManual,
    RecordingGame { exe_name: String, pid: u32 },
}

#[derive(Debug, Clone)]
pub struct EngineStatus {
    pub state: RecordingState,
    pub buffered_secs: f64,
    pub buffer_ram_mb: f64,
    pub last_clip: Option<PathBuf>,
}

impl Default for EngineStatus {
    fn default() -> Self {
        Self {
            state: RecordingState::Idle,
            buffered_secs: 0.0,
            buffer_ram_mb: 0.0,
            last_clip: None,
        }
    }
}

/// Activa resolución de temporizador de 1 ms en Windows solo mientras se graba
/// (garantiza un ritmo constante de 60.000 / 30.000 FPS sin micro-tirones).
struct HighResTimerGuard;

impl HighResTimerGuard {
    fn new() -> Self {
        unsafe {
            timeBeginPeriod(1);
        }
        Self
    }
}

impl Drop for HighResTimerGuard {
    fn drop(&mut self) {
        unsafe {
            timeEndPeriod(1);
        }
    }
}

/// Sesión activa de grabación en Replay Buffer a tasa de cuadros constante (CFR).
///
/// Solo existe mientras se graba; al cerrarse se liberan GPU, audio y el timer de 1 ms (0% GPU).
struct ActiveSession {
    capture: WgcCapture,
    converter: GpuNv12Converter,
    encoder: H264Encoder,
    audio: Option<AudioPipeline>,
    replay_buffer: ReplayBuffer,
    video_config: VideoConfig,
    /// Reloj QPC de inicio de la sesión para alinear video CFR (0, dt, 2*dt...) con el audio.
    qpc_origin_100ns: i64,
    /// Índice del próximo frame CFR a emitir.
    frame_index: u64,
    /// Última textura NV12 ya convertida en VRAM (se reutiliza sin repetir `VideoProcessorBlt`
    /// si el juego/escritorio no presentó un frame nuevo en el tick actual).
    last_nv12: Option<ID3D11Texture2D>,
    state: RecordingState,
    _timer_guard: HighResTimerGuard,
}

impl ActiveSession {
    fn start(
        d3d: &SharedD3DDevice,
        cfg: &AppConfig,
        target: CaptureTarget,
        audio_source: AudioSource,
        state: RecordingState,
    ) -> Result<Self> {
        let timer_guard = HighResTimerGuard::new();
        let video_config = cfg.video_config();
        let capture = WgcCapture::start(d3d, target, cfg.capture_cursor)?;
        let (out_w, out_h) = video_config.resolution.dimensions();

        let converter = GpuNv12Converter::new(
            d3d,
            capture.initial_size.Width as u32,
            capture.initial_size.Height as u32,
            out_w,
            out_h,
            video_config.fps.as_u32(),
        )?;
        let encoder = H264Encoder::new(d3d, video_config)?;

        let max_bytes = cfg.max_buffer_ram_mb.max(32) * 1024 * 1024;
        let mut replay_buffer = ReplayBuffer::new(cfg.replay_duration.as_secs(), max_bytes);

        let audio = match AudioPipeline::start(cfg.audio_config(audio_source)) {
            Ok(pipe) => {
                replay_buffer.set_audio_info(pipe.stream_info.clone());
                Some(pipe)
            }
            Err(err) => {
                if matches!(audio_source, AudioSource::ProcessLoopback { .. }) {
                    warn!("ProcessLoopback falló ({err:#}); reintentando con SystemLoopback");
                    match AudioPipeline::start(cfg.audio_config(AudioSource::SystemLoopback)) {
                        Ok(pipe) => {
                            replay_buffer.set_audio_info(pipe.stream_info.clone());
                            Some(pipe)
                        }
                        Err(err2) => {
                            warn!("Audio no disponible ({err2:#}); grabando solo video");
                            None
                        }
                    }
                } else {
                    warn!("Audio no disponible ({err:#}); grabando solo video");
                    None
                }
            }
        };

        let qpc_origin_100ns = clock::now_100ns();

        Ok(Self {
            capture,
            converter,
            encoder,
            audio,
            replay_buffer,
            video_config,
            qpc_origin_100ns,
            frame_index: 0,
            last_nv12: None,
            state,
            _timer_guard: timer_guard,
        })
    }

    fn pump_audio(&mut self) {
        if let Some(audio) = &self.audio {
            while let Ok(apkt) = audio.rx.try_recv() {
                self.replay_buffer.push_audio(apkt);
            }
        }
    }

    /// Espera con precisión de sub-milisegundo hasta el siguiente tick CFR (60.000 / 30.000 FPS)
    /// y codifica el frame más reciente de la GPU (o reutiliza el último NV12 si la imagen no cambió).
    fn step_cfr_tick(&mut self) -> Result<()> {
        let fps = self.video_config.fps.as_u32() as i64;
        let target_rel_100ns = (self.frame_index as i64 * 10_000_000) / fps;
        let target_qpc = self.qpc_origin_100ns + target_rel_100ns;

        // Espera híbrida: sleep de 1 ms mientras falte >1.5 ms, luego yield fino para exactitud CFR.
        loop {
            let now = clock::now_100ns();
            let remaining = target_qpc - now;
            if remaining <= 0 {
                // Si el hilo sufrió una pausa externa (>120 ms), resincronizar índice de frame
                // sin ráfagas de frames duplicados.
                if remaining < -1_200_000 {
                    let elapsed = (now - self.qpc_origin_100ns).max(0);
                    self.frame_index = ((elapsed * fps) / 10_000_000) as u64;
                }
                break;
            }
            if remaining > 15_000 {
                // > 1.5 ms
                let sleep_ms = ((remaining - 10_000) / 10_000).clamp(1, 8) as u64;
                thread::sleep(Duration::from_millis(sleep_ms));
            } else {
                thread::yield_now();
            }
        }

        // 1. Tomar el frame más reciente del buzón WGC (si llegó uno nuevo desde el tick anterior).
        if let Some(fresh) = self.capture.take_latest() {
            let nv12 = self
                .converter
                .convert(&fresh.texture, fresh.width, fresh.height)?;
            self.last_nv12 = Some(nv12);
        }

        // 2. Codificar exactamente 1 frame CFR en este tick (si ya recibimos al menos el primer frame).
        if let Some(nv12) = self.last_nv12.as_ref() {
            let rel_pts = (self.frame_index as i64 * 10_000_000) / fps;
            let pkts = self
                .encoder
                .encode_nv12_texture(nv12, rel_pts, self.qpc_origin_100ns)?;
            for pkt in pkts {
                self.replay_buffer.push_video(pkt);
            }
            self.frame_index += 1;
        } else {
            // Todavía esperando el primer frame inicial de WGC; resincronizar origen al llegar.
            self.qpc_origin_100ns = clock::now_100ns();
        }

        Ok(())
    }
}

/// Controlador principal del motor en segundo plano de Bals.
pub struct RecorderEngine {
    pub cmd_tx: Sender<EngineCommand>,
    pub event_rx: Receiver<EngineEvent>,
    pub status: Arc<Mutex<EngineStatus>>,
    handle: Option<JoinHandle<()>>,
}

impl RecorderEngine {
    pub fn spawn(initial_config: AppConfig) -> Result<Self> {
        let (cmd_tx, cmd_rx) = unbounded::<EngineCommand>();
        let (event_tx, event_rx) = unbounded::<EngineEvent>();
        let status = Arc::new(Mutex::new(EngineStatus::default()));
        let status_clone = Arc::clone(&status);

        let handle = thread::Builder::new()
            .name("bals-engine".to_string())
            .spawn(move || {
                if let Err(err) = unsafe { RoInitialize(RO_INIT_MULTITHREADED) } {
                    error!("RoInitialize falló en bals-engine: {err}");
                    return;
                }
                engine_loop(initial_config, cmd_rx, event_tx, status_clone);
                unsafe { RoUninitialize() };
            })
            .context("No se pudo iniciar el hilo bals-engine")?;

        Ok(Self {
            cmd_tx,
            event_rx,
            status,
            handle: Some(handle),
        })
    }

    pub fn shutdown(mut self) {
        let _ = self.cmd_tx.send(EngineCommand::Shutdown);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

fn engine_loop(
    mut cfg: AppConfig,
    cmd_rx: Receiver<EngineCommand>,
    event_tx: Sender<EngineEvent>,
    shared_status: Arc<Mutex<EngineStatus>>,
) {
    let d3d = match SharedD3DDevice::new() {
        Ok(d) => d,
        Err(err) => {
            error!("No se pudo inicializar D3D11 en bals-engine: {err:#}");
            return;
        }
    };

    let mut session: Option<ActiveSession> = None;
    let mut last_game_poll = Instant::now() - Duration::from_secs(10);
    let mut last_status_publish = Instant::now();

    info!(
        mode = ?cfg.capture_mode,
        resolution = %cfg.resolution,
        fps = %cfg.fps,
        replay = %cfg.replay_duration,
        hotkey = %cfg.save_clip_hotkey,
        "Motor CFR de Bals listo en segundo plano"
    );

    loop {
        // 1. Procesar todos los comandos pendientes sin bloquear.
        let mut should_exit = false;
        while let Ok(cmd) = cmd_rx.try_recv() {
            match cmd {
                EngineCommand::Shutdown => {
                    should_exit = true;
                    break;
                }
                EngineCommand::StartManual => {
                    start_manual_session(&d3d, &cfg, &mut session);
                    publish_status(&session, &shared_status, &event_tx);
                }
                EngineCommand::Stop => {
                    if session.take().is_some() {
                        info!("Grabación detenida (GPU en reposo 0%)");
                        publish_status(&session, &shared_status, &event_tx);
                    }
                }
                EngineCommand::ToggleManual => {
                    if session.is_some() {
                        session = None;
                        info!("Grabación pausada manualmente");
                    } else {
                        start_manual_session(&d3d, &cfg, &mut session);
                    }
                    publish_status(&session, &shared_status, &event_tx);
                }
                EngineCommand::SaveClip => {
                    handle_save_clip(&mut session, &cfg, &shared_status, &event_tx);
                }
                EngineCommand::UpdateConfig(new_cfg) => {
                    let needs_restart = new_cfg.resolution != cfg.resolution
                        || new_cfg.fps != cfg.fps
                        || new_cfg.replay_duration != cfg.replay_duration
                        || new_cfg.mic_enabled != cfg.mic_enabled
                        || new_cfg.capture_cursor != cfg.capture_cursor
                        || (new_cfg.game_volume - cfg.game_volume).abs() > 0.001
                        || (new_cfg.mic_volume - cfg.mic_volume).abs() > 0.001;

                    let prev_state = session.as_ref().map(|s| s.state.clone());
                    cfg = new_cfg;
                    if let Err(err) = cfg.save() {
                        warn!("Error guardando config.toml: {err:#}");
                    }

                    if needs_restart {
                        if let Some(prev) = prev_state {
                            session = None;
                            match prev {
                                RecordingState::RecordingManual => {
                                    start_manual_session(&d3d, &cfg, &mut session);
                                }
                                RecordingState::RecordingGame { .. } => {
                                    last_game_poll = Instant::now() - Duration::from_secs(10);
                                }
                                RecordingState::Idle => {}
                            }
                            publish_status(&session, &shared_status, &event_tx);
                        }
                    }
                }
            }
        }

        if should_exit {
            break;
        }

        // 2. Detección automática de juegos cada 2 segundos (si está activa).
        if cfg.capture_mode == CaptureMode::AutoDetectGames
            && last_game_poll.elapsed() >= Duration::from_secs(2)
        {
            last_game_poll = Instant::now();
            poll_game_autodetect(&d3d, &cfg, &mut session, &shared_status, &event_tx);
        }

        // 3. Ejecutar el siguiente tick CFR exacto si hay sesión activa, o dormir si estamos en reposo.
        if let Some(active) = session.as_mut() {
            active.pump_audio();
            if let Err(err) = active.step_cfr_tick() {
                warn!("La sesión de captura se cerró ({err:#}); pasando a reposo");
                session = None;
                publish_status(&session, &shared_status, &event_tx);
            }
        } else if let Ok(cmd) = cmd_rx.recv_timeout(Duration::from_millis(200)) {
            match cmd {
                EngineCommand::Shutdown => break,
                EngineCommand::StartManual | EngineCommand::ToggleManual => {
                    start_manual_session(&d3d, &cfg, &mut session);
                    publish_status(&session, &shared_status, &event_tx);
                }
                EngineCommand::Stop => {}
                EngineCommand::SaveClip => {
                    let _ = event_tx.send(EngineEvent::ClipFailed(
                        "No hay ninguna grabación activa. Inicia un juego o activa el modo manual."
                            .to_string(),
                    ));
                }
                EngineCommand::UpdateConfig(new_cfg) => {
                    cfg = new_cfg;
                    let _ = cfg.save();
                }
            }
        }

        // 4. Actualizar telemetría (segundos en buffer y RAM usada) cada ~1 s.
        if last_status_publish.elapsed() >= Duration::from_secs(1) {
            last_status_publish = Instant::now();
            update_shared_metrics(&session, &shared_status);
        }
    }

    drop(session);
    info!("Motor de Bals apagado limpiamente");
}

fn start_manual_session(
    d3d: &SharedD3DDevice,
    cfg: &AppConfig,
    session: &mut Option<ActiveSession>,
) {
    *session = None;
    match ActiveSession::start(
        d3d,
        cfg,
        CaptureTarget::PrimaryMonitor,
        AudioSource::SystemLoopback,
        RecordingState::RecordingManual,
    ) {
        Ok(active) => {
            info!(
                resolution = %cfg.resolution,
                fps = %cfg.fps,
                replay = %cfg.replay_duration,
                "Replay Buffer manual CFR iniciado (monitor principal)"
            );
            *session = Some(active);
        }
        Err(err) => {
            error!("No se pudo iniciar la grabación manual: {err:#}");
        }
    }
}

fn poll_game_autodetect(
    d3d: &SharedD3DDevice,
    cfg: &AppConfig,
    session: &mut Option<ActiveSession>,
    shared_status: &Arc<Mutex<EngineStatus>>,
    event_tx: &Sender<EngineEvent>,
) {
    if let Some(active) = session.as_ref() {
        match &active.state {
            RecordingState::RecordingManual => return,
            RecordingState::RecordingGame { pid, exe_name } => {
                if is_process_alive(*pid) {
                    return;
                }
                info!(game = %exe_name, pid, "El juego se cerró; pausando Replay Buffer (0% GPU)");
                *session = None;
                publish_status(session, shared_status, event_tx);
            }
            RecordingState::Idle => {}
        }
    }

    if let Some(DetectedGame {
        hwnd,
        pid,
        exe_name,
    }) = detect_foreground_game(&cfg.extra_game_exes)
    {
        info!(game = %exe_name, pid, "Juego detectado automáticamente; iniciando Replay Buffer CFR");
        let audio_src = AudioSource::ProcessLoopback {
            pid,
            include_tree: true,
        };
        let state = RecordingState::RecordingGame {
            exe_name: exe_name.clone(),
            pid,
        };
        match ActiveSession::start(d3d, cfg, CaptureTarget::Window(hwnd), audio_src, state) {
            Ok(active) => {
                *session = Some(active);
                publish_status(session, shared_status, event_tx);
            }
            Err(err) => {
                warn!(game = %exe_name, "No se pudo enganchar ventana WGC ({err:#}); intentando monitor principal");
                let fallback_state = RecordingState::RecordingGame { exe_name, pid };
                if let Ok(active) = ActiveSession::start(
                    d3d,
                    cfg,
                    CaptureTarget::PrimaryMonitor,
                    audio_src,
                    fallback_state,
                ) {
                    *session = Some(active);
                    publish_status(session, shared_status, event_tx);
                }
            }
        }
    }
}

fn handle_save_clip(
    session: &mut Option<ActiveSession>,
    cfg: &AppConfig,
    shared_status: &Arc<Mutex<EngineStatus>>,
    event_tx: &Sender<EngineEvent>,
) {
    let Some(active) = session.as_mut() else {
        let _ = event_tx.send(EngineEvent::ClipFailed(
            "No hay grabación activa en este momento.".to_string(),
        ));
        return;
    };

    active.pump_audio();

    let Some(snapshot) = active
        .replay_buffer
        .snapshot(Some(cfg.replay_duration.as_secs()))
    else {
        let _ = event_tx.send(EngineEvent::ClipFailed(
            "El buffer aún no tiene suficiente video (espera 2 segundos).".to_string(),
        ));
        return;
    };

    let clip_path = generate_clip_path(&cfg.clips_dir);
    let video_config = active.video_config;
    let event_tx_clone = event_tx.clone();
    let status_clone = Arc::clone(shared_status);

    let _ = thread::Builder::new()
        .name("bals-mp4-writer".to_string())
        .spawn(move || match write_mp4_clip(&clip_path, &snapshot, video_config) {
            Ok(stats) => {
                if let Ok(mut guard) = status_clone.lock() {
                    guard.last_clip = Some(stats.path.clone());
                }
                let _ = event_tx_clone.send(EngineEvent::ClipSaved(stats));
            }
            Err(err) => {
                error!("Error escribiendo clip MP4: {err:#}");
                let _ = event_tx_clone.send(EngineEvent::ClipFailed(format!("{err:#}")));
            }
        });
}

fn update_shared_metrics(
    session: &Option<ActiveSession>,
    shared_status: &Arc<Mutex<EngineStatus>>,
) {
    if let Ok(mut guard) = shared_status.lock() {
        if let Some(active) = session {
            guard.state = active.state.clone();
            guard.buffered_secs = active.replay_buffer.buffered_secs();
            guard.buffer_ram_mb = active.replay_buffer.total_bytes() as f64 / (1024.0 * 1024.0);
        } else {
            guard.state = RecordingState::Idle;
            guard.buffered_secs = 0.0;
            guard.buffer_ram_mb = 0.0;
        }
    }
}

fn publish_status(
    session: &Option<ActiveSession>,
    shared_status: &Arc<Mutex<EngineStatus>>,
    event_tx: &Sender<EngineEvent>,
) {
    update_shared_metrics(session, shared_status);
    if let Ok(guard) = shared_status.lock() {
        let _ = event_tx.send(EngineEvent::StatusChanged(guard.clone()));
    }
}
