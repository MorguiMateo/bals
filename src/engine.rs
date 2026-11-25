use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use crossbeam_channel::{Receiver, Sender, unbounded};
use tracing::{error, info, warn};
use windows::Win32::System::WinRT::{RO_INIT_MULTITHREADED, RoInitialize, RoUninitialize};

use crate::audio::AudioPipeline;
use crate::buffer::ReplayBuffer;
use crate::capture::CaptureTarget;
use crate::capture::wgc::WgcCapture;
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

/// Sesión activa de grabación en Replay Buffer (solo existe mientras se está grabando;
/// cuando está en `None`, el uso de GPU es exactamente 0%).
struct ActiveSession {
    capture: WgcCapture,
    converter: GpuNv12Converter,
    encoder: H264Encoder,
    audio: Option<AudioPipeline>,
    replay_buffer: ReplayBuffer,
    video_config: VideoConfig,
    min_step_100ns: i64,
    last_encoded_ts: Option<i64>,
    state: RecordingState,
}

impl ActiveSession {
    fn start(
        d3d: &SharedD3DDevice,
        cfg: &AppConfig,
        target: CaptureTarget,
        audio_source: AudioSource,
        state: RecordingState,
    ) -> Result<Self> {
        let video_config = cfg.video_config();
        let capture = WgcCapture::start(d3d, target, cfg.capture_cursor, 2)?;
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

        // Intentar iniciar el audio pedido; si ProcessLoopback falla en un juego específico,
        // hacemos fallback automático a SystemLoopback para nunca dejar un clip mudo.
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

        let min_step_100ns = (video_config.fps.frame_duration_100ns() * 85) / 100;

        Ok(Self {
            capture,
            converter,
            encoder,
            audio,
            replay_buffer,
            video_config,
            min_step_100ns,
            last_encoded_ts: None,
            state,
        })
    }

    fn pump_audio(&mut self) {
        if let Some(audio) = &self.audio {
            while let Ok(apkt) = audio.rx.try_recv() {
                self.replay_buffer.push_audio(apkt);
            }
        }
    }

    fn step_video(&mut self, timeout: Duration) -> Result<()> {
        match self.capture.rx.recv_timeout(timeout) {
            Ok(frame) => {
                if let Some(prev) = self.last_encoded_ts {
                    if frame.timestamp_100ns.saturating_sub(prev) < self.min_step_100ns {
                        return Ok(());
                    }
                }
                self.last_encoded_ts = Some(frame.timestamp_100ns);

                let nv12 = self
                    .converter
                    .convert(&frame.texture, frame.width, frame.height)?;
                let pkts = self
                    .encoder
                    .encode_nv12_texture(&nv12, frame.timestamp_100ns)?;
                for pkt in pkts {
                    self.replay_buffer.push_video(pkt);
                }
            }
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => {
                anyhow::bail!("Canal WGC desconectado");
            }
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

    // Si el usuario eligió modo manual, arranca listo para que pulse "Iniciar" (o si elige AutoDetect, empieza vigilando juegos).
    info!(
        mode = ?cfg.capture_mode,
        resolution = %cfg.resolution,
        fps = %cfg.fps,
        replay = %cfg.replay_duration,
        hotkey = %cfg.save_clip_hotkey,
        "Motor de Bals listo en segundo plano"
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

                    // Si cambió a modo manual mientras grababa un juego auto-detectado, dejamos al usuario el control.
                    if needs_restart {
                        if let Some(prev) = prev_state {
                            session = None;
                            match prev {
                                RecordingState::RecordingManual => {
                                    start_manual_session(&d3d, &cfg, &mut session);
                                }
                                RecordingState::RecordingGame { .. } => {
                                    // En el próximo sondeo inmediato re-enganchará el juego con la nueva config.
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

        // 3. Procesar el siguiente frame si hay sesión activa, o dormir tranquilamente si estamos en reposo.
        if let Some(active) = session.as_mut() {
            active.pump_audio();
            if let Err(err) = active.step_video(Duration::from_millis(20)) {
                warn!("La sesión de captura se cerró ({err:#}); pasando a reposo");
                session = None;
                publish_status(&session, &shared_status, &event_tx);
            }
        } else {
            // En reposo esperamos comandos con timeout de 200 ms → 0% GPU y ~0.0% CPU.
            if let Ok(cmd) = cmd_rx.recv_timeout(Duration::from_millis(200)) {
                // Re-encolamos procesando inmediatamente en la siguiente vuelta? Mejor manejarlo aquí:
                match cmd {
                    EngineCommand::Shutdown => break,
                    EngineCommand::StartManual | EngineCommand::ToggleManual => {
                        start_manual_session(&d3d, &cfg, &mut session);
                        publish_status(&session, &shared_status, &event_tx);
                    }
                    EngineCommand::Stop => {}
                    EngineCommand::SaveClip => {
                        let _ = event_tx.send(EngineEvent::ClipFailed(
                            "No hay ninguna grabación activa. Inicia un juego o activa el modo manual.".to_string(),
                        ));
                    }
                    EngineCommand::UpdateConfig(new_cfg) => {
                        cfg = new_cfg;
                        let _ = cfg.save();
                    }
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
                "Replay Buffer manual iniciado (monitor principal)"
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
    // Si ya estamos grabando un juego, verificamos primero si el proceso sigue vivo.
    // (Así un Alt-Tab breve no borra los últimos 30 segundos del buffer.)
    if let Some(active) = session.as_ref() {
        match &active.state {
            RecordingState::RecordingManual => return, // No interferir con modo manual activo.
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

    // Buscar si hay un juego nuevo en primer plano.
    if let Some(DetectedGame {
        hwnd,
        pid,
        exe_name,
    }) = detect_foreground_game(&cfg.extra_game_exes)
    {
        info!(game = %exe_name, pid, "Juego detectado automáticamente; iniciando Replay Buffer");
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

    // Escribir el MP4 en un hilo dedicado de fondo para 0 interrupciones durante el juego.
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
