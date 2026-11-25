mod clips_view;
mod settings_view;
mod theme;
mod widgets;

use std::path::{Path, PathBuf};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU64, Ordering},
};
use std::time::{Duration, Instant, SystemTime};

use anyhow::Result;
use eframe::egui::{self, Align2, Color32, FontId, Rect, Sense, Stroke, StrokeKind, pos2, vec2};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
use windows::core::{HSTRING, PCWSTR, w};

use crate::audio::meter::AudioLevelMonitor;
use crate::capture::{MonitorInfo, list_monitors};
use crate::config::{AppConfig, EncoderPreference, UiTheme};
use crate::encode::h264_mft::{DetectedEncoders, detect_system_encoders};
use crate::game_detect::{RunningApp, list_running_window_apps};
use crate::hotkey::parse_hotkey;
use crate::ipc::{self, EngineTelemetrySnapshot, TelemetryState};
use crate::startup;
use crate::trimmer::{
    ClipMetadata, PreviewFrame, TrimStats, compress_mp4_balanced, extract_mp4_frame_rgba,
    probe_mp4_metadata, suggested_output_with_suffix, suggested_trim_output, trim_mp4_passthrough,
};
use crate::ui::theme::{RetroPalette, apply_egui_theme};
use crate::ui::widgets::{
    TrimDragHandle, draw_crt_recess, draw_mini_bezel_vu, draw_scanlines, draw_status_led,
    draw_vent_grille, hardware_action_button, hardware_tab_button,
};
use crate::updater::{self, CURRENT_VERSION, ReleaseInfo};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Tab {
    Settings,
    Clips,
}

#[derive(Debug, Clone)]
pub(crate) struct ClipEntry {
    pub path: PathBuf,
    pub name: String,
    /// Subcarpeta del juego si el clip fue clasificado por juego (ej. `Some("Valorant")`).
    pub folder: Option<String>,
    pub size_mb: f64,
    pub modified: SystemTime,
}

pub(crate) enum TrimState {
    Idle,
    Running(String),
    Done(TrimStats, bool),
    Error(String),
}

pub(crate) enum UpdateUiState {
    Idle,
    Checking,
    UpToDate,
    Available(ReleaseInfo),
    Downloading,
    Error(String),
}

struct PreviewResult {
    request_id: u64,
    frame: PreviewFrame,
}

pub(crate) struct BalsUiApp {
    pub tab: Tab,
    pub cfg: AppConfig,
    pub applied_theme: Option<UiTheme>,
    pub start_with_windows: bool,
    pub hotkey_error: Option<String>,
    pub listening_hotkey: bool,
    pub show_exe_picker: bool,
    pub running_apps: Vec<RunningApp>,
    pub exe_picker_filter: String,
    pub banner_msg: Option<(String, bool, Instant)>,

    // Estado en vivo del daemon vía memoria compartida
    pub daemon_running: bool,
    pub telemetry: Option<EngineTelemetrySnapshot>,
    pub last_telemetry_poll: Instant,
    pub last_clip_event_seq: u32,

    // Auto-updater, encoders detectados, monitores conectados y audio VU continuo
    pub detected_encoders: DetectedEncoders,
    pub monitors: Vec<MonitorInfo>,
    pub update_state: Arc<Mutex<UpdateUiState>>,
    pub audio_meter: AudioLevelMonitor,
    pub desktop_vu_display: f32,
    pub mic_vu_display: f32,
    last_vu_tick: Instant,

    // Logo pixel-art CRT embebido
    pub logo_texture: Option<egui::TextureHandle>,

    // Galería, Monitor CRT y recortador
    pub clips: Vec<ClipEntry>,
    pub clip_filter: String,
    pub selected_clip: Option<PathBuf>,
    pub selected_metadata: Option<ClipMetadata>,
    pub selected_duration: Option<f64>,
    pub trim_start: f64,
    pub trim_end: f64,
    pub trim_drag_handle: Option<TrimDragHandle>,
    pub trim_state: Arc<Mutex<TrimState>>,
    pub confirm_delete: Option<PathBuf>,

    // Vista previa de frame CRT asíncrona con debounce
    pub preview_texture: Option<egui::TextureHandle>,
    pub preview_time_secs: f64,
    pub preview_loading: bool,
    preview_req_counter: Arc<AtomicU64>,
    preview_ready: Arc<Mutex<Option<PreviewResult>>>,
    pending_preview_seek: Option<(f64, Instant)>,
}

impl BalsUiApp {
    fn new(ctx: &egui::Context) -> Self {
        let mut cfg = AppConfig::load_or_create();
        let start_with_windows = startup::is_autostart_enabled();
        let clips = scan_clips(&cfg.clips_dir);
        let daemon_running = ipc::is_daemon_running();
        let telemetry = ipc::read_engine_telemetry();
        let last_clip_event_seq = telemetry.as_ref().map_or(0, |t| t.clip_event_seq);
        let detected_encoders = detect_system_encoders();
        if cfg.encoder_pref == EncoderPreference::HardwareGpu
            && detected_encoders.hw_name.is_none()
        {
            cfg.encoder_pref = EncoderPreference::SoftwareCpu;
        }
        let monitors = list_monitors();

        // Cargar ícono CRT 64×64 pixel-art con filtrado NEAREST (sin blur).
        let raw_icon = include_bytes!("../../assets/icon_64.rgba");
        let logo_img = egui::ColorImage::from_rgba_unmultiplied([64, 64], raw_icon);
        let logo_texture = Some(ctx.load_texture(
            "bals_crt_logo",
            logo_img,
            egui::TextureOptions::NEAREST,
        ));

        let mut app = Self {
            tab: Tab::Settings,
            cfg,
            applied_theme: None,
            start_with_windows,
            hotkey_error: None,
            listening_hotkey: false,
            show_exe_picker: false,
            running_apps: Vec::new(),
            exe_picker_filter: String::new(),
            banner_msg: None,
            daemon_running,
            telemetry,
            last_telemetry_poll: Instant::now(),
            last_clip_event_seq,
            detected_encoders,
            monitors,
            update_state: Arc::new(Mutex::new(UpdateUiState::Idle)),
            audio_meter: AudioLevelMonitor::start(),
            desktop_vu_display: 0.0,
            mic_vu_display: 0.0,
            last_vu_tick: Instant::now(),
            logo_texture,
            clips,
            clip_filter: String::new(),
            selected_clip: None,
            selected_metadata: None,
            selected_duration: None,
            trim_start: 0.0,
            trim_end: 10.0,
            trim_drag_handle: None,
            trim_state: Arc::new(Mutex::new(TrimState::Idle)),
            confirm_delete: None,
            preview_texture: None,
            preview_time_secs: 0.0,
            preview_loading: false,
            preview_req_counter: Arc::new(AtomicU64::new(0)),
            preview_ready: Arc::new(Mutex::new(None)),
            pending_preview_seek: None,
        };

        if let Some(first) = app.clips.first().cloned() {
            app.select_clip(first.path);
        }
        app
    }

    pub fn set_banner(&mut self, msg: impl Into<String>, is_err: bool) {
        self.banner_msg = Some((msg.into(), is_err, Instant::now()));
    }

    /// Activa o desactiva la captura de una nueva hotkey pausando temporalmente la hotkey global
    /// del daemon para que presionar la tecla actual no dispare un clip.
    pub fn set_listening_hotkey(&mut self, listening: bool) {
        if self.listening_hotkey == listening {
            return;
        }
        self.listening_hotkey = listening;
        if listening {
            ipc::notify_pause_hotkey();
        } else {
            ipc::notify_resume_hotkey();
        }
    }

    pub fn open_exe_picker(&mut self) {
        self.set_listening_hotkey(false);
        self.refresh_running_apps();
        self.exe_picker_filter.clear();
        self.show_exe_picker = true;
    }

    pub fn refresh_running_apps(&mut self) {
        self.running_apps = list_running_window_apps();
    }

    pub fn toggle_extra_exe(&mut self, exe: &str) {
        let trimmed = exe.trim();
        if trimmed.is_empty() {
            return;
        }
        if let Some(idx) = self
            .cfg
            .extra_game_exes
            .iter()
            .position(|g| g.eq_ignore_ascii_case(trimmed))
        {
            self.cfg.extra_game_exes.remove(idx);
        } else {
            self.cfg.extra_game_exes.push(trimmed.to_string());
        }
    }

    pub fn refresh_monitors(&mut self) {
        self.monitors = list_monitors();
    }

    pub fn refresh_clips(&mut self) {
        self.clips = scan_clips(&self.cfg.clips_dir);
        if let Some(sel) = &self.selected_clip {
            if !sel.exists() {
                self.selected_clip = None;
                self.selected_metadata = None;
                self.selected_duration = None;
                self.preview_texture = None;
            }
        }
        if self.selected_clip.is_none() {
            if let Some(first) = self.clips.first().cloned() {
                self.select_clip(first.path);
            }
        }
    }

    pub fn select_clip(&mut self, path: PathBuf) {
        let meta = probe_mp4_metadata(&path).ok();
        let dur = meta.map(|m| m.duration_secs);
        self.selected_clip = Some(path);
        self.selected_metadata = meta;
        self.selected_duration = dur;
        self.trim_start = 0.0;
        self.trim_end = dur.unwrap_or(10.0);
        self.confirm_delete = None;
        self.preview_texture = None;
        self.preview_time_secs = 0.0;
        if let Ok(mut st) = self.trim_state.lock() {
            *st = TrimState::Idle;
        }
        self.request_preview_frame(0.2_f64.min(dur.unwrap_or(0.0)));
    }

    pub fn schedule_preview_seek(&mut self, time_secs: f64) {
        self.preview_time_secs = time_secs;
        self.pending_preview_seek = Some((time_secs, Instant::now()));
    }

    fn request_preview_frame(&mut self, time_secs: f64) {
        let Some(path) = self.selected_clip.clone() else {
            return;
        };
        let req_id = self.preview_req_counter.fetch_add(1, Ordering::SeqCst) + 1;
        let counter = Arc::clone(&self.preview_req_counter);
        let ready = Arc::clone(&self.preview_ready);
        self.preview_loading = true;
        self.preview_time_secs = time_secs;

        std::thread::Builder::new()
            .name("bals-crt-preview".to_string())
            .spawn(move || {
                // Cancelación temprana si ya llegó una solicitud más nueva.
                if counter.load(Ordering::Relaxed) != req_id {
                    return;
                }
                if let Ok(frame) = extract_mp4_frame_rgba(&path, time_secs, 448, 252) {
                    if counter.load(Ordering::Relaxed) == req_id {
                        if let Ok(mut slot) = ready.lock() {
                            *slot = Some(PreviewResult {
                                request_id: req_id,
                                frame,
                            });
                        }
                    }
                }
            })
            .ok();
    }

    pub fn save_config(&mut self) {
        if let Err(err) = parse_hotkey(&self.cfg.save_clip_hotkey) {
            self.hotkey_error = Some(format!("{err}"));
            self.set_banner("Corrige el atajo de teclado antes de guardar.", true);
            return;
        }
        self.hotkey_error = None;
        self.set_listening_hotkey(false);

        if let Err(err) = startup::set_autostart_enabled(self.start_with_windows) {
            self.set_banner(format!("Error configurando inicio Windows: {err:#}"), true);
            return;
        }

        match self.cfg.save() {
            Ok(()) => {
                self.daemon_running = ipc::is_daemon_running();
                if ipc::notify_reload_config() {
                    self.set_banner("CONFIG GUARDADA Y APLICADA EN VIVO.", false);
                } else {
                    self.set_banner(
                        "Config guardada, pero Bals no está activo en la bandeja.",
                        true,
                    );
                }
            }
            Err(err) => {
                self.set_banner(format!("Error guardando config: {err:#}"), true);
            }
        }
    }

    pub fn start_trim(&mut self) {
        let Some(input_path) = self.selected_clip.clone() else {
            return;
        };
        let output_path = suggested_trim_output(&input_path);
        let start = self.trim_start;
        let end = self.trim_end;
        let state_arc = Arc::clone(&self.trim_state);

        if let Ok(mut st) = state_arc.lock() {
            if matches!(*st, TrimState::Running(_)) {
                return;
            }
            *st = TrimState::Running("RECORTANDO...".to_string());
        }

        std::thread::spawn(move || {
            let res = trim_mp4_passthrough(&input_path, &output_path, start, end);
            if let Ok(mut st) = state_arc.lock() {
                *st = match res {
                    Ok(stats) => TrimState::Done(stats, false),
                    Err(err) => TrimState::Error(format!("{err:#}")),
                };
            }
        });
    }

    pub fn start_compress_balanced(&mut self) {
        let Some(input_path) = self.selected_clip.clone() else {
            return;
        };
        let output_path = suggested_output_with_suffix(&input_path, "comp");
        let start = self.trim_start;
        let end = self.trim_end;
        let state_arc = Arc::clone(&self.trim_state);

        if let Ok(mut st) = state_arc.lock() {
            if matches!(*st, TrimState::Running(_)) {
                return;
            }
            *st = TrimState::Running("COMPRIMIENDO...".to_string());
        }
        self.set_banner(
            "Comprimiendo clip (~35% menos peso en calidad original)...",
            false,
        );

        std::thread::spawn(move || {
            let res = compress_mp4_balanced(&input_path, &output_path, start, end);
            if let Ok(mut st) = state_arc.lock() {
                *st = match res {
                    Ok(stats) => TrimState::Done(stats, true),
                    Err(err) => TrimState::Error(format!("{err:#}")),
                };
            }
        });
    }

    pub fn check_for_updates(&self) {
        let state_arc = Arc::clone(&self.update_state);
        if let Ok(mut st) = state_arc.lock() {
            if matches!(*st, UpdateUiState::Checking | UpdateUiState::Downloading) {
                return;
            }
            *st = UpdateUiState::Checking;
        }
        std::thread::spawn(move || {
            let next = match updater::check_latest_release() {
                Ok(Some(info)) => UpdateUiState::Available(info),
                Ok(None) => UpdateUiState::UpToDate,
                Err(err) => UpdateUiState::Error(format!("{err:#}")),
            };
            if let Ok(mut st) = state_arc.lock() {
                *st = next;
            }
        });
    }

    pub fn install_update(&self, info: ReleaseInfo) {
        let state_arc = Arc::clone(&self.update_state);
        if let Ok(mut st) = state_arc.lock() {
            *st = UpdateUiState::Downloading;
        }
        std::thread::spawn(move || {
            // Descargar y verificar integridad del ejecutable ANTES de detener el daemon.
            let bytes = match updater::download_update_bytes(&info) {
                Ok(b) => b,
                Err(err) => {
                    if let Ok(mut st) = state_arc.lock() {
                        *st = UpdateUiState::Error(format!("{err:#}"));
                    }
                    return;
                }
            };
            ipc::notify_shutdown();
            // Esperar a que el daemon libere la ventana de bandeja, GPU y hotkey antes de relanzar.
            let _ = ipc::wait_until_daemon_stopped(Duration::from_secs(4));
            match updater::apply_downloaded_update(&bytes, &info, &[]) {
                Ok(()) => std::process::exit(0),
                Err(err) => {
                    if let Ok(mut st) = state_arc.lock() {
                        *st = UpdateUiState::Error(format!("{err:#}"));
                    }
                }
            }
        });
    }

    fn poll_background_states(&mut self, ctx: &egui::Context) -> Option<String> {
        // 1. Telemetría compartida daemon → UI cada 200 ms (0 I/O en disco).
        if self.last_telemetry_poll.elapsed() >= Duration::from_millis(200) {
            self.last_telemetry_poll = Instant::now();
            self.daemon_running = ipc::is_daemon_running();
            if self.daemon_running {
                if let Some(tel) = ipc::read_engine_telemetry() {
                    if tel.clip_event_seq != self.last_clip_event_seq && tel.clip_event_seq > 0 {
                        self.last_clip_event_seq = tel.clip_event_seq;
                        if !tel.last_event.is_empty() {
                            self.set_banner(tel.last_event.clone(), tel.clip_event_is_err);
                        }
                        if !tel.clip_event_is_err {
                            self.refresh_clips();
                        }
                    }
                    self.telemetry = Some(tel);
                }
            } else {
                self.telemetry = None;
            }
        }

        // 2. Preview CRT debounced (90 ms tras arrastrar [IN]/[OUT]).
        if let Some((seek_t, at)) = self.pending_preview_seek {
            if at.elapsed() >= Duration::from_millis(90) {
                self.pending_preview_seek = None;
                self.request_preview_frame(seek_t);
            } else {
                ctx.request_repaint_after(Duration::from_millis(40));
            }
        }

        // 3. Cargar textura si el worker terminó de decodificar el frame CRT.
        if let Ok(mut slot) = self.preview_ready.lock() {
            if let Some(res) = slot.take() {
                if res.request_id == self.preview_req_counter.load(Ordering::Relaxed) {
                    let img = egui::ColorImage::from_rgba_unmultiplied(
                        [res.frame.width as usize, res.frame.height as usize],
                        &res.frame.rgba,
                    );
                    self.preview_texture = Some(ctx.load_texture(
                        "bals_crt_preview",
                        img,
                        egui::TextureOptions::LINEAR,
                    ));
                    self.preview_time_secs = res.frame.actual_time_secs;
                    self.preview_loading = false;
                }
            }
        }
        if self.preview_loading {
            ctx.request_repaint_after(Duration::from_millis(60));
        }

        // 4. Estado del trimmer / compresor en segundo plano.
        let mut finished_trim_msg: Option<(String, bool)> = None;
        let mut running_op: Option<String> = None;
        if let Ok(mut st) = self.trim_state.lock() {
            match &*st {
                TrimState::Running(label) => running_op = Some(label.clone()),
                TrimState::Done(stats, was_compress) => {
                    let name = stats
                        .output_path
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    let in_mb = stats.input_bytes as f64 / (1024.0 * 1024.0);
                    let out_mb = stats.output_bytes as f64 / (1024.0 * 1024.0);
                    let msg = if *was_compress && stats.input_bytes > 0 {
                        let pct = ((1.0 - (out_mb / in_mb)) * 100.0).round() as i32;
                        format!(
                            "COMPRIMIDO: {name} ({in_mb:.1} -> {out_mb:.1} MB · -{pct}%)"
                        )
                    } else {
                        format!(
                            "OK ({:.0}ms): {name} ({:.1}s · {out_mb:.1} MB)",
                            stats.elapsed_ms, stats.duration_secs
                        )
                    };
                    finished_trim_msg = Some((msg, false));
                    *st = TrimState::Idle;
                }
                TrimState::Error(msg) => {
                    finished_trim_msg = Some((format!("ERROR: {msg}"), true));
                    *st = TrimState::Idle;
                }
                TrimState::Idle => {}
            }
        }
        if running_op.is_some() {
            ctx.request_repaint_after(Duration::from_millis(80));
        }
        if let Some((msg, is_err)) = finished_trim_msg {
            self.set_banner(msg, is_err);
            self.refresh_clips();
        }

        // 5. Limpiar toast temporal tras 6 segundos.
        if let Some((_, _, at)) = &self.banner_msg {
            if at.elapsed() > Duration::from_secs(6) {
                self.banner_msg = None;
            }
        }

        // 6. Actualizar barras VU con ataque instantáneo y decaimiento continuo uniforme (~0.35 s).
        let dt = self.last_vu_tick.elapsed().as_secs_f32().clamp(0.0, 0.25);
        self.last_vu_tick = Instant::now();
        let decay = dt * 2.8;

        let d_raw = (self.audio_meter.desktop_level() * self.cfg.game_volume).min(1.0);
        self.desktop_vu_display = if d_raw >= self.desktop_vu_display {
            d_raw
        } else {
            (self.desktop_vu_display - decay).max(d_raw)
        };

        let m_raw = if self.cfg.mic_enabled {
            (self.audio_meter.mic_level() * self.cfg.mic_volume).min(1.0)
        } else {
            0.0
        };
        self.mic_vu_display = if m_raw >= self.mic_vu_display {
            m_raw
        } else {
            (self.mic_vu_display - decay).max(m_raw)
        };

        // Pausar cadencia alta del VU cuando la ventana pierde foco (y restaurar hotkey si se escuchaba).
        let focused = ctx.input(|i| i.focused);
        self.audio_meter.set_paused(!focused);
        if !focused && self.listening_hotkey {
            self.set_listening_hotkey(false);
        }

        running_op
    }

    fn handle_keyboard_shortcuts(&mut self, ctx: &egui::Context) {
        // Si estamos capturando una tecla nueva para el atajo global:
        if self.listening_hotkey {
            let captured = ctx.input(|i| {
                for ev in &i.events {
                    if let egui::Event::Key {
                        key,
                        pressed: true,
                        modifiers,
                        ..
                    } = ev
                    {
                        if *key == egui::Key::Escape {
                            return Some(None);
                        }
                        if let Some(spec) = egui_key_to_hotkey_spec(*key, *modifiers) {
                            return Some(Some(spec));
                        }
                    }
                }
                None
            });

            if let Some(res) = captured {
                self.set_listening_hotkey(false);
                if let Some(spec) = res {
                    self.cfg.save_clip_hotkey = spec;
                    self.hotkey_error = parse_hotkey(&self.cfg.save_clip_hotkey)
                        .err()
                        .map(|e| format!("{e}"));
                }
            }
            return;
        }

        // No disparar atajos de pestañas mientras el selector modal de `.exe` esté abierto.
        if self.show_exe_picker {
            return;
        }

        // Atajos de consola cuando ningún cuadro de texto tiene foco.
        let text_editing = ctx.egui_wants_keyboard_input();
        let (switch_settings, switch_clips, save_now, toggle_rec) = ctx.input(|i| {
            // Evitar disparos repetidos al mantener presionado Ctrl+S (`repeat == false`).
            let ctrl_s = i.events.iter().any(|ev| {
                matches!(
                    ev,
                    egui::Event::Key {
                        key: egui::Key::S,
                        pressed: true,
                        repeat: false,
                        modifiers,
                        ..
                    } if modifiers.ctrl && !modifiers.alt && !modifiers.shift
                )
            });
            let f1 = i.key_pressed(egui::Key::F1);
            let f2 = i.key_pressed(egui::Key::F2);
            let f5 = i.key_pressed(egui::Key::F5);
            let q = !text_editing && !i.modifiers.any() && i.key_pressed(egui::Key::Q);
            let e = !text_editing && !i.modifiers.any() && i.key_pressed(egui::Key::E);
            (f1 || q, f2 || e, ctrl_s, f5)
        });

        if switch_settings {
            self.tab = Tab::Settings;
        }
        if switch_clips {
            self.set_listening_hotkey(false);
            if self.tab != Tab::Clips {
                self.refresh_clips();
            }
            self.tab = Tab::Clips;
        }
        if save_now && self.tab == Tab::Settings {
            self.save_config();
        }
        if toggle_rec && self.daemon_running {
            ipc::notify_toggle_record();
            self.set_banner("Estado de grabación alternado.", false);
        }
    }
}

/// Abre la ventana nativa liviana de Bals (`eframe` + `glow`).
/// Al cerrar la ventana, el proceso `--ui` termina y libera el 100% de su RAM y contexto GPU.
pub fn run_gui() -> Result<()> {
    let icon = egui::IconData {
        rgba: include_bytes!("../../assets/icon_64.rgba").to_vec(),
        width: 64,
        height: 64,
    };

    let options = eframe::NativeOptions {
        renderer: eframe::Renderer::Glow,
        viewport: egui::ViewportBuilder::default()
            .with_title("Bals")
            .with_icon(Arc::new(icon))
            .with_inner_size([1080.0, 710.0])
            .with_min_inner_size([1000.0, 700.0]),
        ..Default::default()
    };

    eframe::run_native(
        "Bals",
        options,
        Box::new(|cc| {
            let app = BalsUiApp::new(&cc.egui_ctx);
            let p = RetroPalette::for_theme(app.cfg.ui_theme);
            apply_egui_theme(&cc.egui_ctx, &p);
            Ok(Box::new(app))
        }),
    )
    .map_err(|e| anyhow::anyhow!("Error ejecutando ventana UI de Bals: {e}"))
}

impl eframe::App for BalsUiApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();

        // Reaplicar estilo al instante si el usuario cambió el tema retro.
        if self.applied_theme != Some(self.cfg.ui_theme) {
            let p = RetroPalette::for_theme(self.cfg.ui_theme);
            apply_egui_theme(&ctx, &p);
            self.applied_theme = Some(self.cfg.ui_theme);
        }

        let p = RetroPalette::for_theme(self.cfg.ui_theme);
        self.handle_keyboard_shortcuts(&ctx);
        let running_op = self.poll_background_states(&ctx);

        // Refresco fluido para VU meters y pulso LED (~25 FPS activo, ~5 FPS en segundo plano).
        let repaint_ms = if ctx.input(|i| i.focused) { 40 } else { 200 };
        ctx.request_repaint_after(Duration::from_millis(repaint_ms));

        let time_s = ctx.input(|i| i.time) as f32;
        let rec_pulse = ((time_s * 3.8).sin() * 0.5 + 0.5).clamp(0.0, 1.0);

        // =========================================================================
        // 1. BISEL SUPERIOR DE CARCASA HARDWARE CRT (Opción C)
        // =========================================================================
        let top_frame = egui::Frame::new()
            .fill(p.chassis_bg)
            .inner_margin(egui::Margin::symmetric(14, 8))
            .stroke(Stroke::new(1.5, p.chassis_outline));

        egui::Panel::top("bals_crt_top_bezel")
            .frame(top_frame)
            .show_separator_line(false)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    // Ícono pixel-art CRT + Branding BALS
                    if let Some(tex) = &self.logo_texture {
                        let (lrect, _) = ui.allocate_exact_size(vec2(38.0, 38.0), Sense::hover());
                        ui.painter().image(
                            tex.id(),
                            lrect,
                            Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
                            Color32::WHITE,
                        );
                    }
                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 0.0;
                        ui.label(
                            egui::RichText::new("BALS")
                                .strong()
                                .size(19.0)
                                .color(p.chassis_text),
                        );
                        ui.label(
                            egui::RichText::new(format!("CRT-60 v{CURRENT_VERSION}"))
                                .size(10.0)
                                .color(p.chassis_muted),
                        );
                    });

                    ui.add_space(10.0);

                    // Pestañas físicas estilo gatillo consola [L1] / [R1]
                    if hardware_tab_button(ui, &p, self.tab == Tab::Settings, "L1", "AJUSTES")
                        .clicked()
                    {
                        self.tab = Tab::Settings;
                    }
                    let clips_lbl = format!("CLIPS & DECK ({})", self.clips.len());
                    if hardware_tab_button(ui, &p, self.tab == Tab::Clips, "R1", &clips_lbl)
                        .clicked()
                    {
                        self.set_listening_hotkey(false);
                        self.tab = Tab::Clips;
                        self.refresh_clips();
                    }

                    // Lado derecho: LCD de telemetría en vivo + botones físicos + LED power
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        // LED verde físico de encendido
                        let (led_rect, led_resp) =
                            ui.allocate_exact_size(vec2(14.0, 14.0), Sense::hover());
                        draw_status_led(
                            ui.painter(),
                            led_rect.center(),
                            4.5,
                            p.led_green,
                            self.daemon_running,
                            0.4,
                        );
                        led_resp.on_hover_text(if self.daemon_running {
                            "Daemon activo en bandeja"
                        } else {
                            "Daemon detenido"
                        });

                        if self.daemon_running {
                            let is_rec = self
                                .telemetry
                                .as_ref()
                                .is_some_and(|t| t.state != TelemetryState::Idle);
                            let rec_lbl = if is_rec { "⏸ PAUSA" } else { "▶ REC" };
                            if hardware_action_button(
                                ui,
                                &p,
                                rec_lbl,
                                p.chassis_btn_bg,
                                p.chassis_text,
                                30.0,
                            )
                            .on_hover_text("Iniciar o pausar Replay Buffer manual (F5)")
                            .clicked()
                            {
                                ipc::notify_toggle_record();
                                self.set_banner("Estado de grabación alternado.", false);
                            }

                            let clip_btn_lbl = format!("[{}] CLIP", self.cfg.save_clip_hotkey);
                            if hardware_action_button(
                                ui,
                                &p,
                                &clip_btn_lbl,
                                p.danger,
                                p.danger_text,
                                30.0,
                            )
                            .on_hover_text("Guardar los últimos segundos del Replay Buffer")
                            .clicked()
                            {
                                ipc::notify_save_clip();
                                self.set_banner("Solicitud de clip enviada al motor.", false);
                            }
                        }

                        // Pantalla LCD empotrada con telemetría en tiempo real
                        draw_live_telemetry_lcd(ui, &p, self.daemon_running, &self.telemetry, rec_pulse);
                    });
                });
            });

        // =========================================================================
        // 2. BISEL INFERIOR DE CARCASA HARDWARE CRT (Opción C + Leyenda Consola A)
        // =========================================================================
        let bottom_frame = egui::Frame::new()
            .fill(p.chassis_bg)
            .inner_margin(egui::Margin::symmetric(14, 6))
            .stroke(Stroke::new(1.5, p.chassis_outline));

        egui::Panel::bottom("bals_crt_bottom_bezel")
            .frame(bottom_frame)
            .show_separator_line(false)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    // Rejillas físicas de ventilación a la izquierda
                    draw_vent_grille(ui, &p, 56.0, 22.0);
                    ui.add_space(6.0);

                    // Si hay una notificación activa, mostrarla en un visor LCD; si no, mostrar mini-VU en vivo
                    if let Some((msg, is_err, _)) = &self.banner_msg {
                        let (brect, _) = ui.allocate_exact_size(vec2(380.0, 24.0), Sense::hover());
                        ui.painter().rect_filled(brect, 3.0, p.crt_recess);
                        let col = if *is_err { p.led_red } else { p.accent_bright };
                        ui.painter().rect_stroke(
                            brect,
                            3.0,
                            Stroke::new(1.2, col),
                            StrokeKind::Inside,
                        );
                        ui.painter().text(
                            pos2(brect.left() + 8.0, brect.center().y),
                            Align2::LEFT_CENTER,
                            msg,
                            FontId::monospace(11.0),
                            col,
                        );
                    } else {
                        draw_mini_bezel_vu(
                            ui,
                            &p,
                            self.desktop_vu_display,
                            self.mic_vu_display,
                            self.cfg.mic_enabled,
                        );

                        ui.add_space(8.0);
                        ui.label(
                            egui::RichText::new("[Q/E] PESTAÑAS · [CTRL+S] GUARDAR · [F5] REC")
                                .size(10.5)
                                .color(p.chassis_muted),
                        );
                    }

                    // Badge derecho: estado de GPU / Encoder
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let enc_text = self
                            .telemetry
                            .as_ref()
                            .filter(|_| self.daemon_running)
                            .map(|t| t.encoder_label.as_str())
                            .unwrap_or("MOTOR DETENIDO");
                        let badge_w = (enc_text.chars().count() as f32 * 6.8 + 26.0).max(125.0);
                        let (erect, _) =
                            ui.allocate_exact_size(vec2(badge_w, 24.0), Sense::hover());
                        ui.painter().rect_filled(erect, 3.0, p.crt_recess);
                        ui.painter().rect_stroke(
                            erect,
                            3.0,
                            Stroke::new(1.2, p.chassis_outline),
                            StrokeKind::Inside,
                        );
                        draw_status_led(
                            ui.painter(),
                            pos2(erect.left() + 11.0, erect.center().y),
                            3.2,
                            if self.daemon_running {
                                p.led_green
                            } else {
                                p.led_red
                            },
                            true,
                            0.2,
                        );
                        ui.painter().text(
                            pos2(erect.left() + 21.0, erect.center().y),
                            Align2::LEFT_CENTER,
                            enc_text,
                            FontId::monospace(10.5),
                            p.crt_text,
                        );
                    });
                });
            });

        // =========================================================================
        // 3. PANTALLA CRT EMPOTRADA CENTRAL (Contorno Chasis + Vidrio CRT + Scanlines)
        // =========================================================================
        let outer_center_frame = egui::Frame::new()
            .fill(p.chassis_bg)
            .inner_margin(egui::Margin::symmetric(14, 5));

        egui::CentralPanel::default()
            .frame(outer_center_frame)
            .show(ui, |ui| {
                let crt_rect = ui.available_rect_before_wrap();
                draw_crt_recess(ui.painter(), crt_rect, &p);

                let inner_frame = egui::Frame::new()
                    .fill(Color32::TRANSPARENT)
                    .inner_margin(egui::Margin::same(10));

                inner_frame.show(ui, |ui| {
                    match self.tab {
                        Tab::Settings => settings_view::render_settings_tab(self, ui, &p),
                        Tab::Clips => clips_view::render_clips_tab(
                            self,
                            ui,
                            &p,
                            running_op.as_deref(),
                        ),
                    }
                });

                // Scanlines CRT sutiles encima de la pantalla interior (opcional)
                if self.cfg.crt_scanlines {
                    draw_scanlines(ui.painter(), crt_rect.shrink(4.0), p.crt_scanline);
                }
            });
    }
}

impl Drop for BalsUiApp {
    fn drop(&mut self) {
        if self.listening_hotkey {
            ipc::notify_resume_hotkey();
        }
    }
}

/// Dibuja el visor LCD oscuro empotrado en el bisel superior con estado en vivo del motor.
fn draw_live_telemetry_lcd(
    ui: &mut egui::Ui,
    p: &RetroPalette,
    daemon_running: bool,
    telemetry: &Option<EngineTelemetrySnapshot>,
    rec_pulse: f32,
) {
    let (label, led_color, is_rec) = if !daemon_running {
        ("MOTOR DETENIDO".to_string(), p.led_red, false)
    } else if let Some(t) = telemetry {
        match t.state {
            TelemetryState::Idle => ("STANDBY · 0% GPU".to_string(), p.led_green, false),
            TelemetryState::RecordingManual => {
                let target = if t.game_name.is_empty() {
                    "MONITOR"
                } else {
                    t.game_name.as_str()
                };
                (
                    format!(
                        "{target} {:.0}s/{}s · {:.0}MB",
                        t.buffered_secs, t.replay_target_secs, t.buffer_ram_mb
                    ),
                    p.led_red,
                    true,
                )
            }
            TelemetryState::RecordingGame => {
                let g = if t.game_name.len() > 12 {
                    &t.game_name[..12]
                } else {
                    &t.game_name
                };
                (
                    format!("{g} {:.0}s · {:.0}MB", t.buffered_secs, t.buffer_ram_mb),
                    p.led_red,
                    true,
                )
            }
            TelemetryState::PausedAltTab => (
                format!("ALT-TAB {:.0}s · HOLD", t.buffered_secs),
                p.amber,
                false,
            ),
        }
    } else {
        ("STANDBY · 0% GPU".to_string(), p.led_green, false)
    };

    let width = (label.chars().count() as f32 * 6.9 + 28.0).clamp(142.0, 218.0);
    let (rect, _) = ui.allocate_exact_size(vec2(width, 30.0), Sense::hover());
    let painter = ui.painter();

    painter.rect_filled(rect, 3.0, p.crt_recess);
    painter.rect_stroke(
        rect,
        3.0,
        Stroke::new(1.5, p.chassis_outline),
        StrokeKind::Inside,
    );

    draw_status_led(
        painter,
        pos2(rect.left() + 12.0, rect.center().y),
        3.6,
        led_color,
        true,
        if is_rec { rec_pulse } else { 0.15 },
    );
    painter.text(
        pos2(rect.left() + 23.0, rect.center().y),
        Align2::LEFT_CENTER,
        label,
        FontId::monospace(11.0),
        if is_rec { p.led_red } else { p.crt_text },
    );
}

fn egui_key_to_hotkey_spec(key: egui::Key, mods: egui::Modifiers) -> Option<String> {
    let base = match key {
        egui::Key::F1 => "F1",
        egui::Key::F2 => "F2",
        egui::Key::F3 => "F3",
        egui::Key::F4 => "F4",
        egui::Key::F5 => "F5",
        egui::Key::F6 => "F6",
        egui::Key::F7 => "F7",
        egui::Key::F8 => "F8",
        egui::Key::F9 => "F9",
        egui::Key::F10 => "F10",
        egui::Key::F11 => "F11",
        egui::Key::F12 => "F12",
        egui::Key::F13 => "F13",
        egui::Key::F14 => "F14",
        egui::Key::F15 => "F15",
        egui::Key::F16 => "F16",
        egui::Key::F17 => "F17",
        egui::Key::F18 => "F18",
        egui::Key::F19 => "F19",
        egui::Key::F20 => "F20",
        egui::Key::Backslash => "\\",
        egui::Key::Slash => "/",
        egui::Key::Minus => "-",
        egui::Key::Equals => "=",
        egui::Key::OpenBracket => "[",
        egui::Key::CloseBracket => "]",
        egui::Key::Backtick => "`",
        egui::Key::Insert => "Insert",
        egui::Key::Home => "Home",
        egui::Key::End => "End",
        egui::Key::PageUp => "PgUp",
        egui::Key::PageDown => "PgDn",
        egui::Key::A => "A",
        egui::Key::B => "B",
        egui::Key::C => "C",
        egui::Key::D => "D",
        egui::Key::E => "E",
        egui::Key::F => "F",
        egui::Key::G => "G",
        egui::Key::H => "H",
        egui::Key::I => "I",
        egui::Key::J => "J",
        egui::Key::K => "K",
        egui::Key::L => "L",
        egui::Key::M => "M",
        egui::Key::N => "N",
        egui::Key::O => "O",
        egui::Key::P => "P",
        egui::Key::Q => "Q",
        egui::Key::R => "R",
        egui::Key::S => "S",
        egui::Key::T => "T",
        egui::Key::U => "U",
        egui::Key::V => "V",
        egui::Key::W => "W",
        egui::Key::X => "X",
        egui::Key::Y => "Y",
        egui::Key::Z => "Z",
        egui::Key::Num0 => "0",
        egui::Key::Num1 => "1",
        egui::Key::Num2 => "2",
        egui::Key::Num3 => "3",
        egui::Key::Num4 => "4",
        egui::Key::Num5 => "5",
        egui::Key::Num6 => "6",
        egui::Key::Num7 => "7",
        egui::Key::Num8 => "8",
        egui::Key::Num9 => "9",
        _ => return None,
    };

    let mut parts: Vec<&str> = Vec::with_capacity(4);
    if mods.ctrl {
        parts.push("Ctrl");
    }
    if mods.alt {
        parts.push("Alt");
    }
    if mods.shift {
        parts.push("Shift");
    }
    parts.push(base);
    Some(parts.join("+"))
}

fn scan_clips(dir: &Path) -> Vec<ClipEntry> {
    let mut clips = Vec::new();
    collect_mp4_in_dir(dir, None, &mut clips);

    // Escanear también 1 nivel de subcarpetas de juegos (ej. `clips/Valorant/*.mp4`).
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let sub = entry.path();
            if sub.is_dir() {
                let folder_name = sub
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .filter(|s| !s.is_empty());
                if folder_name.is_some() {
                    collect_mp4_in_dir(&sub, folder_name, &mut clips);
                }
            }
        }
    }

    clips.sort_by(|a, b| b.modified.cmp(&a.modified));
    clips
}

fn collect_mp4_in_dir(dir: &Path, folder: Option<String>, out: &mut Vec<ClipEntry>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let is_mp4 = path
            .extension()
            .is_some_and(|e| e.to_ascii_lowercase() == "mp4");
        if !is_mp4 {
            continue;
        }
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        let modified = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
        let Some(name) = path.file_name().map(|n| n.to_string_lossy().into_owned()) else {
            continue;
        };
        out.push(ClipEntry {
            path,
            name,
            folder: folder.clone(),
            size_mb: meta.len() as f64 / (1024.0 * 1024.0),
            modified,
        });
    }
}

pub(crate) fn open_in_shell(path: &Path) {
    let target = HSTRING::from(path.as_os_str());
    unsafe {
        ShellExecuteW(
            None,
            w!("open"),
            &target,
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        );
    }
}
