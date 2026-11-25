use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use anyhow::Result;
use eframe::egui;
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
use windows::core::{HSTRING, PCWSTR, w};

use crate::audio::meter::AudioLevelMonitor;
use crate::config::{
    AppConfig, AudioCaptureMode, CaptureMode, FrameRate, QualityPreset, ReplayDuration, Resolution,
    VideoConfig,
};
use crate::hotkey::parse_hotkey;
use crate::ipc;
use crate::startup;
use crate::trimmer::{
    TrimStats, compress_mp4_to_target_mb, probe_mp4_duration_secs, suggested_output_with_suffix,
    suggested_trim_output, trim_mp4_passthrough,
};
use crate::updater::{self, CURRENT_VERSION, ReleaseInfo};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tab {
    Settings,
    Clips,
}

#[derive(Debug, Clone)]
struct ClipEntry {
    path: PathBuf,
    name: String,
    size_mb: f64,
    modified: SystemTime,
}

enum TrimState {
    Idle,
    Running,
    Done(TrimStats),
    Error(String),
}

enum UpdateUiState {
    Idle,
    Checking,
    UpToDate,
    Available(ReleaseInfo),
    Downloading,
    Error(String),
}

struct BalsUiApp {
    tab: Tab,
    cfg: AppConfig,
    start_with_windows: bool,
    extra_games_text: String,
    hotkey_error: Option<String>,
    banner_msg: Option<(String, bool)>,
    daemon_running: bool,
    update_state: Arc<Mutex<UpdateUiState>>,
    audio_meter: AudioLevelMonitor,

    // Galería y recortador
    clips: Vec<ClipEntry>,
    selected_clip: Option<PathBuf>,
    selected_duration: Option<f64>,
    trim_start: f64,
    trim_end: f64,
    trim_state: Arc<Mutex<TrimState>>,
}

impl BalsUiApp {
    fn new() -> Self {
        let cfg = AppConfig::load_or_create();
        let start_with_windows = startup::is_autostart_enabled();
        let extra_games_text = cfg.extra_game_exes.join(", ");
        let clips = scan_clips(&cfg.clips_dir);
        let daemon_running = ipc::is_daemon_running();

        let mut app = Self {
            tab: Tab::Settings,
            cfg,
            start_with_windows,
            extra_games_text,
            hotkey_error: None,
            banner_msg: None,
            daemon_running,
            update_state: Arc::new(Mutex::new(UpdateUiState::Idle)),
            audio_meter: AudioLevelMonitor::start(),
            clips,
            selected_clip: None,
            selected_duration: None,
            trim_start: 0.0,
            trim_end: 10.0,
            trim_state: Arc::new(Mutex::new(TrimState::Idle)),
        };

        if let Some(first) = app.clips.first().cloned() {
            app.select_clip(first.path);
        }
        app
    }

    fn refresh_clips(&mut self) {
        self.clips = scan_clips(&self.cfg.clips_dir);
        if let Some(sel) = &self.selected_clip {
            if !sel.exists() {
                self.selected_clip = None;
                self.selected_duration = None;
            }
        }
    }

    fn select_clip(&mut self, path: PathBuf) {
        let dur = probe_mp4_duration_secs(&path).ok();
        self.selected_clip = Some(path);
        self.selected_duration = dur;
        self.trim_start = 0.0;
        self.trim_end = dur.unwrap_or(10.0);
        if let Ok(mut st) = self.trim_state.lock() {
            *st = TrimState::Idle;
        }
    }

    fn save_config(&mut self) {
        if let Err(err) = parse_hotkey(&self.cfg.save_clip_hotkey) {
            self.hotkey_error = Some(format!("{err}"));
            self.banner_msg = Some(("Corrige el atajo de teclado antes de guardar.".into(), true));
            return;
        }
        self.hotkey_error = None;

        self.cfg.extra_game_exes = self
            .extra_games_text
            .split([',', ';', '\n'])
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();

        if let Err(err) = startup::set_autostart_enabled(self.start_with_windows) {
            self.banner_msg = Some((format!("Error configurando inicio con Windows: {err:#}"), true));
            return;
        }

        match self.cfg.save() {
            Ok(()) => {
                self.daemon_running = ipc::is_daemon_running();
                if ipc::notify_reload_config() {
                    self.banner_msg = Some(("Ajustes guardados y aplicados al instante.".into(), false));
                } else {
                    self.banner_msg = Some((
                        "Ajustes guardados en config.toml (inicia Bals en bandeja para aplicar).".into(),
                        false,
                    ));
                }
            }
            Err(err) => {
                self.banner_msg = Some((format!("Error guardando config: {err:#}"), true));
            }
        }
    }

    fn start_trim(&mut self) {
        let Some(input_path) = self.selected_clip.clone() else {
            return;
        };
        let output_path = suggested_trim_output(&input_path);
        let start = self.trim_start;
        let end = self.trim_end;
        let state_arc = Arc::clone(&self.trim_state);

        if let Ok(mut st) = state_arc.lock() {
            if matches!(*st, TrimState::Running) {
                return;
            }
            *st = TrimState::Running;
        }

        std::thread::spawn(move || {
            let res = trim_mp4_passthrough(&input_path, &output_path, start, end);
            if let Ok(mut st) = state_arc.lock() {
                *st = match res {
                    Ok(stats) => TrimState::Done(stats),
                    Err(err) => TrimState::Error(format!("{err:#}")),
                };
            }
        });
    }

    fn start_compress(&mut self, target_mb: f64, suffix: &'static str) {
        let Some(input_path) = self.selected_clip.clone() else {
            return;
        };
        let output_path = suggested_output_with_suffix(&input_path, suffix);
        let start = self.trim_start;
        let end = self.trim_end;
        let state_arc = Arc::clone(&self.trim_state);

        if let Ok(mut st) = state_arc.lock() {
            if matches!(*st, TrimState::Running) {
                return;
            }
            *st = TrimState::Running;
        }

        std::thread::spawn(move || {
            let res = compress_mp4_to_target_mb(&input_path, &output_path, target_mb, start, end);
            if let Ok(mut st) = state_arc.lock() {
                *st = match res {
                    Ok(stats) => TrimState::Done(stats),
                    Err(err) => TrimState::Error(format!("{err:#}")),
                };
            }
        });
    }

    fn check_for_updates(&self) {
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

    fn install_update(&self, info: ReleaseInfo) {
        let state_arc = Arc::clone(&self.update_state);
        if let Ok(mut st) = state_arc.lock() {
            *st = UpdateUiState::Downloading;
        }
        std::thread::spawn(move || {
            ipc::notify_shutdown();
            std::thread::sleep(std::time::Duration::from_millis(250));
            match updater::download_and_apply_update(&info, &[]) {
                Ok(()) => std::process::exit(0),
                Err(err) => {
                    if let Ok(mut st) = state_arc.lock() {
                        *st = UpdateUiState::Error(format!("{err:#}"));
                    }
                }
            }
        });
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
            .with_inner_size([800.0, 560.0])
            .with_min_inner_size([660.0, 460.0]),
        ..Default::default()
    };

    eframe::run_native(
        "Bals",
        options,
        Box::new(|cc| {
            configure_style(&cc.egui_ctx);
            Ok(Box::new(BalsUiApp::new()))
        }),
    )
    .map_err(|e| anyhow::anyhow!("Error ejecutando ventana UI de Bals: {e}"))
}

fn configure_style(ctx: &egui::Context) {
    let mut visuals = egui::Visuals::dark();
    visuals.selection.bg_fill = egui::Color32::from_rgb(16, 185, 129); // verde esmeralda Bals
    visuals.selection.stroke = egui::Stroke::new(1.0, egui::Color32::WHITE);
    visuals.widgets.active.bg_fill = egui::Color32::from_rgb(5, 150, 105);
    ctx.set_visuals(visuals);
}

impl eframe::App for BalsUiApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        // Si hay un recorte en progreso, comprobar si terminó y refrescar la galería.
        let mut finished_trim_msg: Option<(String, bool)> = None;
        let mut trim_in_progress = false;
        if let Ok(mut st) = self.trim_state.lock() {
            match &*st {
                TrimState::Running => {
                    trim_in_progress = true;
                }
                TrimState::Done(stats) => {
                    let name = stats
                        .output_path
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    finished_trim_msg = Some((
                        format!(
                            "Clip recortado en {:.0} ms: {name} ({:.1} s · {:.1} MB)",
                            stats.elapsed_ms,
                            stats.duration_secs,
                            stats.output_bytes as f64 / (1024.0 * 1024.0)
                        ),
                        false,
                    ));
                    *st = TrimState::Idle;
                }
                TrimState::Error(msg) => {
                    finished_trim_msg = Some((format!("Error recortando: {msg}"), true));
                    *st = TrimState::Idle;
                }
                TrimState::Idle => {}
            }
        }
        if trim_in_progress {
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(80));
        }
        if let Some(msg) = finished_trim_msg {
            self.banner_msg = Some(msg);
            self.refresh_clips();
        }

        // Barra superior
        egui::Panel::top("bals_header").show(ui, |ui| {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.heading(
                    egui::RichText::new("Bals")
                        .strong()
                        .color(egui::Color32::from_rgb(52, 211, 153)),
                );
                ui.label(egui::RichText::new(format!("v{CURRENT_VERSION}")).weak());

                ui.separator();
                ui.selectable_value(&mut self.tab, Tab::Settings, "⚙ Ajustes");
                if ui
                    .selectable_value(
                        &mut self.tab,
                        Tab::Clips,
                        format!("🎬 Clips y Recortador ({})", self.clips.len()),
                    )
                    .clicked()
                {
                    self.refresh_clips();
                }

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if self.daemon_running {
                        if ui
                            .button(format!("💾 Guardar clip [{}]", self.cfg.save_clip_hotkey))
                            .clicked()
                        {
                            ipc::notify_save_clip();
                            self.banner_msg =
                                Some(("Solicitud de clip enviada al motor.".into(), false));
                        }
                        if ui.button("⏯ Play / Pausa").clicked() {
                            ipc::notify_toggle_record();
                            self.banner_msg =
                                Some(("Estado de grabación alternado.".into(), false));
                        }
                    } else {
                        ui.label(
                            egui::RichText::new("● Motor detenido")
                                .color(egui::Color32::from_rgb(248, 113, 113)),
                        );
                    }
                });
            });
            ui.add_space(4.0);
        });

        // Barra inferior de estado / notificaciones
        if let Some((msg, is_err)) = &self.banner_msg {
            egui::Panel::bottom("bals_banner").show(ui, |ui| {
                ui.add_space(4.0);
                let color = if *is_err {
                    egui::Color32::from_rgb(248, 113, 113)
                } else {
                    egui::Color32::from_rgb(52, 211, 153)
                };
                ui.label(egui::RichText::new(msg).color(color));
                ui.add_space(4.0);
            });
        }

        egui::CentralPanel::default().show(ui, |ui| match self.tab {
            Tab::Settings => self.render_settings_tab(ui),
            Tab::Clips => self.render_clips_tab(ui, trim_in_progress),
        });
    }
}

impl BalsUiApp {
    fn render_settings_tab(&mut self, ui: &mut egui::Ui) {
        egui::ScrollArea::vertical().show(ui, |ui| {
            ui.add_space(6.0);

            // 1. Video y Rendimiento
            ui.group(|ui| {
                ui.strong("Video (Codificación GPU Zero-Copy)");
                ui.add_space(6.0);

                ui.horizontal(|ui| {
                    ui.label("Resolución:");
                    ui.selectable_value(&mut self.cfg.resolution, Resolution::P720, "720p (Ultra liviano)");
                    ui.selectable_value(&mut self.cfg.resolution, Resolution::P1080, "1080p (Full HD)");
                    ui.selectable_value(&mut self.cfg.resolution, Resolution::P1440, "2K (1440p)");
                });

                ui.horizontal(|ui| {
                    ui.label("Cuadros por segundo:");
                    ui.selectable_value(&mut self.cfg.fps, FrameRate::Fps30, "30 fps");
                    ui.selectable_value(&mut self.cfg.fps, FrameRate::Fps60, "60 fps");
                });

                ui.horizontal(|ui| {
                    ui.label("Calidad / Peso:");
                    ui.selectable_value(
                        &mut self.cfg.quality,
                        QualityPreset::Compact,
                        "Ahorro (-45% peso)",
                    );
                    ui.selectable_value(&mut self.cfg.quality, QualityPreset::Balanced, "Normal");
                    ui.selectable_value(&mut self.cfg.quality, QualityPreset::High, "Alta");
                });

                ui.horizontal(|ui| {
                    ui.label("Duración del clip (Replay Buffer):");
                    ui.selectable_value(&mut self.cfg.replay_duration, ReplayDuration::Sec15, "15 s");
                    ui.selectable_value(&mut self.cfg.replay_duration, ReplayDuration::Sec30, "30 s");
                    ui.selectable_value(&mut self.cfg.replay_duration, ReplayDuration::Sec60, "60 s");
                    ui.selectable_value(&mut self.cfg.replay_duration, ReplayDuration::Sec120, "120 s");
                });

                let bitrate_bps = VideoConfig::recommended_bitrate_for(
                    self.cfg.resolution,
                    self.cfg.fps,
                    self.cfg.quality,
                );
                let est_ram_mb = (bitrate_bps as f64 / 8.0) * self.cfg.replay_duration.as_secs() as f64
                    / (1024.0 * 1024.0);
                ui.label(
                    egui::RichText::new(format!(
                        "Bitrate objetivo: {:.1} Mbps · RAM estimada del buffer: ~{:.1} MB · Uso de disco mientras juegas: 0 MB/s",
                        bitrate_bps as f64 / 1_000_000.0,
                        est_ram_mb
                    ))
                    .small()
                    .weak(),
                );
            });

            ui.add_space(8.0);

            // 2. Modo de captura y Hotkey
            ui.group(|ui| {
                ui.strong("Activación y Atajo Global");
                ui.add_space(6.0);

                ui.horizontal(|ui| {
                    ui.label("Modo:");
                    ui.selectable_value(
                        &mut self.cfg.capture_mode,
                        CaptureMode::AutoDetectGames,
                        "Detectar juegos automáticamente (0% GPU en escritorio)",
                    );
                    ui.selectable_value(
                        &mut self.cfg.capture_mode,
                        CaptureMode::ManualMonitor,
                        "Manual (Play / Pausa)",
                    );
                });

                ui.horizontal(|ui| {
                    ui.label("Tecla para guardar clip:");
                    let resp = ui.add(
                        egui::TextEdit::singleline(&mut self.cfg.save_clip_hotkey)
                            .desired_width(110.0),
                    );
                    if resp.changed() {
                        self.hotkey_error = parse_hotkey(&self.cfg.save_clip_hotkey)
                            .err()
                            .map(|e| format!("{e}"));
                    }
                    ui.label(egui::RichText::new("Ej: \\ , F8 , Alt+F10").weak().small());
                });

                if let Some(err) = &self.hotkey_error {
                    ui.label(
                        egui::RichText::new(err).color(egui::Color32::from_rgb(248, 113, 113)),
                    );
                }

                ui.horizontal(|ui| {
                    ui.checkbox(&mut self.cfg.capture_cursor, "Capturar cursor");
                    ui.checkbox(&mut self.cfg.sound_feedback, "Sonido al guardar clip");
                    ui.checkbox(&mut self.cfg.auto_update, "Actualizar automáticamente");
                });
                ui.checkbox(
                    &mut self.start_with_windows,
                    "Iniciar Bals automáticamente con Windows (en bandeja)",
                );
            });

            ui.add_space(8.0);

            // 3. Audio + Medidores VU en tiempo real
            ui.group(|ui| {
                ui.strong("Audio (WASAPI 48 kHz AAC-LC)");
                ui.add_space(6.0);

                ui.horizontal(|ui| {
                    ui.label("Audio a grabar:");
                    ui.selectable_value(
                        &mut self.cfg.audio_capture_mode,
                        AudioCaptureMode::FullDesktop,
                        "Todo el escritorio (juego + Discord + música)",
                    );
                    ui.selectable_value(
                        &mut self.cfg.audio_capture_mode,
                        AudioCaptureMode::GameOnly,
                        "Solo el juego detectado",
                    );
                });

                ui.add_space(6.0);

                // Juego / Escritorio + barra VU en vivo
                let desktop_lvl = (self.audio_meter.desktop_level() * self.cfg.game_volume).min(1.0);
                ui.horizontal(|ui| {
                    ui.label("Juego / Escritorio:");
                    let mut pct = (self.cfg.game_volume * 100.0).round();
                    if ui
                        .add(egui::Slider::new(&mut pct, 0.0..=200.0).suffix("%"))
                        .changed()
                    {
                        self.cfg.game_volume = pct / 100.0;
                    }
                });
                draw_vu_meter(ui, desktop_lvl, "Nivel juego / escritorio");

                ui.add_space(6.0);
                ui.checkbox(
                    &mut self.cfg.mic_enabled,
                    "Grabar micrófono mezclado con el audio",
                );

                let mic_lvl = if self.cfg.mic_enabled {
                    (self.audio_meter.mic_level() * self.cfg.mic_volume).min(1.0)
                } else {
                    0.0
                };
                ui.add_enabled_ui(self.cfg.mic_enabled, |ui| {
                    ui.horizontal(|ui| {
                        ui.label("Micrófono:         ");
                        let mut pct = (self.cfg.mic_volume * 100.0).round();
                        if ui
                            .add(egui::Slider::new(&mut pct, 0.0..=200.0).suffix("%"))
                            .changed()
                        {
                            self.cfg.mic_volume = pct / 100.0;
                        }
                    });
                    draw_vu_meter(ui, mic_lvl, "Nivel micrófono");
                });

                // Refrescar ~30 veces por segundo mientras el usuario mira los niveles de audio.
                ui.ctx()
                    .request_repaint_after(std::time::Duration::from_millis(33));
            });

            ui.add_space(8.0);

            // 4. Carpeta y juegos extra
            ui.group(|ui| {
                ui.strong("Carpeta de clips y Juegos en modo ventana");
                ui.add_space(6.0);

                ui.horizontal(|ui| {
                    ui.label("Carpeta:");
                    let mut dir_str = self.cfg.clips_dir.to_string_lossy().into_owned();
                    if ui
                        .add(egui::TextEdit::singleline(&mut dir_str).desired_width(420.0))
                        .changed()
                    {
                        self.cfg.clips_dir = PathBuf::from(dir_str);
                    }
                    if ui.button("📂 Abrir").clicked() {
                        let _ = std::fs::create_dir_all(&self.cfg.clips_dir);
                        open_in_shell(&self.cfg.clips_dir);
                    }
                });

                ui.horizontal(|ui| {
                    ui.label("Juegos extra (.exe):");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.extra_games_text)
                            .hint_text("ej: javaw.exe, valorant.exe")
                            .desired_width(420.0),
                    );
                });
            });

            ui.add_space(12.0);
            ui.horizontal(|ui| {
                let save_btn = egui::Button::new(
                    egui::RichText::new("💾 Guardar y aplicar cambios").strong(),
                )
                .fill(egui::Color32::from_rgb(16, 185, 129));
                if ui.add_sized([220.0, 32.0], save_btn).clicked() {
                    self.save_config();
                }
                if ui.button("🔄 Buscar actualizaciones").clicked() {
                    self.check_for_updates();
                }

                // Estado del auto-updater
                let mut pending_install: Option<ReleaseInfo> = None;
                if let Ok(st) = self.update_state.lock() {
                    match &*st {
                        UpdateUiState::Idle => {}
                        UpdateUiState::Checking => {
                            ui.label("Comprobando...");
                            ui.ctx()
                                .request_repaint_after(std::time::Duration::from_millis(120));
                        }
                        UpdateUiState::UpToDate => {
                            ui.label(
                                egui::RichText::new(format!("✓ Estás en la última versión (v{CURRENT_VERSION})"))
                                    .color(egui::Color32::from_rgb(52, 211, 153)),
                            );
                        }
                        UpdateUiState::Available(info) => {
                            if ui
                                .button(format!("⬇ Instalar v{}", info.version))
                                .clicked()
                            {
                                pending_install = Some(info.clone());
                            }
                        }
                        UpdateUiState::Downloading => {
                            ui.label("Descargando actualización...");
                            ui.ctx()
                                .request_repaint_after(std::time::Duration::from_millis(120));
                        }
                        UpdateUiState::Error(e) => {
                            ui.label(
                                egui::RichText::new(format!("Error: {e}"))
                                    .small()
                                    .color(egui::Color32::from_rgb(248, 113, 113)),
                            );
                        }
                    }
                }
                if let Some(info) = pending_install {
                    self.install_update(info);
                }
            });
            ui.add_space(4.0);
            ui.label(
                egui::RichText::new(
                    "Al cerrar esta ventana, Bals sigue en tu bandeja del sistema liberando toda la memoria de la interfaz.",
                )
                .small()
                .weak(),
            );
        });
    }

    fn render_clips_tab(&mut self, ui: &mut egui::Ui, trim_in_progress: bool) {
        ui.columns(2, |cols| {
            // Columna izquierda: lista de clips
            cols[0].horizontal(|ui| {
                ui.strong("Tus clips");
                if ui.button("🔄 Actualizar").clicked() {
                    self.refresh_clips();
                }
                if ui.button("📂 Carpeta").clicked() {
                    let _ = std::fs::create_dir_all(&self.cfg.clips_dir);
                    open_in_shell(&self.cfg.clips_dir);
                }
            });
            cols[0].separator();

            if self.clips.is_empty() {
                cols[0].add_space(12.0);
                cols[0].label(
                    egui::RichText::new(format!(
                        "Aún no hay clips en:\n{}",
                        self.cfg.clips_dir.display()
                    ))
                    .weak(),
                );
            } else {
                let mut clicked_path: Option<PathBuf> = None;
                egui::ScrollArea::vertical().show(&mut cols[0], |ui| {
                    for clip in &self.clips {
                        let is_selected = self.selected_clip.as_ref() == Some(&clip.path);
                        let label = format!("🎬 {}  ({:.1} MB)", clip.name, clip.size_mb);
                        if ui.selectable_label(is_selected, label).clicked() {
                            clicked_path = Some(clip.path.clone());
                        }
                    }
                });
                if let Some(p) = clicked_path {
                    self.select_clip(p);
                }
            }

            // Columna derecha: acciones y recortador sin pérdida
            cols[1].strong("Recortador sin pérdida (Passthrough MP4)");
            cols[1].separator();

            let Some(sel_path) = self.selected_clip.clone() else {
                cols[1].add_space(12.0);
                cols[1].label(egui::RichText::new("Selecciona un clip de la lista para reproducirlo o recortarlo.").weak());
                return;
            };

            let name = sel_path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            cols[1].label(egui::RichText::new(name).strong());

            cols[1].add_space(6.0);
            cols[1].horizontal(|ui| {
                if ui.button("▶ Reproducir").clicked() {
                    open_in_shell(&sel_path);
                }
                if ui.button("🗑 Eliminar").clicked() {
                    if std::fs::remove_file(&sel_path).is_ok() {
                        self.banner_msg = Some(("Clip eliminado.".into(), false));
                        self.refresh_clips();
                    }
                }
            });

            cols[1].add_space(12.0);

            let Some(total_dur) = self.selected_duration else {
                cols[1].label("No se pudo leer la duración del archivo.");
                return;
            };

            cols[1].group(|ui| {
                ui.label(format!("Duración original: {:.2} s", total_dur));
                ui.add_space(6.0);

                ui.horizontal(|ui| {
                    ui.label("Inicio (s):");
                    ui.add(
                        egui::Slider::new(&mut self.trim_start, 0.0..=(total_dur - 0.5).max(0.0))
                            .fixed_decimals(1),
                    );
                });
                if self.trim_end <= self.trim_start + 0.5 {
                    self.trim_end = (self.trim_start + 0.5).min(total_dur);
                }

                ui.horizontal(|ui| {
                    ui.label("Fin (s):   ");
                    ui.add(
                        egui::Slider::new(
                            &mut self.trim_end,
                            (self.trim_start + 0.5).min(total_dur)..=total_dur,
                        )
                        .fixed_decimals(1),
                    );
                });

                // Barra visual simple del rango seleccionado
                let (rect, _) =
                    ui.allocate_exact_size(egui::vec2(ui.available_width(), 14.0), egui::Sense::hover());
                let painter = ui.painter();
                painter.rect_filled(rect, 4.0, egui::Color32::from_gray(45));
                if total_dur > 0.0 {
                    let x0 = rect.left() + rect.width() * (self.trim_start / total_dur) as f32;
                    let x1 = rect.left() + rect.width() * (self.trim_end / total_dur) as f32;
                    let sel_rect = egui::Rect::from_min_max(
                        egui::pos2(x0, rect.top()),
                        egui::pos2(x1.max(x0 + 2.0), rect.bottom()),
                    );
                    painter.rect_filled(sel_rect, 4.0, egui::Color32::from_rgb(16, 185, 129));
                }

                ui.add_space(6.0);
                ui.label(
                    egui::RichText::new(format!(
                        "Duración resultante: {:.1} s (corte instantáneo alineado al keyframe más cercano)",
                        (self.trim_end - self.trim_start).max(0.0)
                    ))
                    .small()
                    .weak(),
                );

                ui.add_space(8.0);
                ui.add_enabled_ui(!trim_in_progress, |ui| {
                    ui.horizontal_wrapped(|ui| {
                        let trim_label = if trim_in_progress {
                            "Procesando..."
                        } else {
                            "✂ Recortar sin pérdida"
                        };
                        if ui.button(trim_label).clicked() {
                            self.start_trim();
                        }
                        if ui
                            .button("📦 Comprimir <10 MB (Discord)")
                            .on_hover_text("Recodifica el rango seleccionado usando tu GPU para quedar por debajo de 10 MB")
                            .clicked()
                        {
                            self.start_compress(9.5, "discord");
                        }
                        if ui
                            .button("📦 Comprimir <25 MB")
                            .on_hover_text("Recodifica el rango seleccionado usando tu GPU para quedar por debajo de 25 MB")
                            .clicked()
                        {
                            self.start_compress(24.0, "25mb");
                        }
                    });
                });
            });
        });
    }
}

fn draw_vu_meter(ui: &mut egui::Ui, level: f32, tooltip: &str) {
    let desired = egui::vec2(ui.available_width().min(420.0), 10.0);
    let (rect, response) = ui.allocate_exact_size(desired, egui::Sense::hover());
    let painter = ui.painter();

    // Fondo oscuro de la barra
    painter.rect_filled(rect, 3.0, egui::Color32::from_gray(35));

    let clamped = level.clamp(0.0, 1.0);
    if clamped > 0.005 {
        let fill_w = rect.width() * clamped;
        let fill_rect = egui::Rect::from_min_size(rect.min, egui::vec2(fill_w, rect.height()));
        let color = if clamped > 0.88 {
            egui::Color32::from_rgb(239, 68, 68) // rojo (saturación)
        } else if clamped > 0.65 {
            egui::Color32::from_rgb(245, 158, 11) // ámbar (alto)
        } else {
            egui::Color32::from_rgb(16, 185, 129) // verde esmeralda (óptimo)
        };
        painter.rect_filled(fill_rect, 3.0, color);
    }

    // Marcas de referencia sutiles (50% y 85%)
    let stroke = egui::Stroke::new(1.0, egui::Color32::from_gray(65));
    for frac in [0.50f32, 0.85f32] {
        let x = rect.left() + rect.width() * frac;
        painter.line_segment([egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())], stroke);
    }

    response.on_hover_text(format!("{tooltip}: {:.0}%", clamped * 100.0));
}

fn scan_clips(dir: &Path) -> Vec<ClipEntry> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut clips: Vec<ClipEntry> = entries
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let path = e.path();
            if path.extension()?.to_ascii_lowercase() != "mp4" {
                return None;
            }
            let meta = e.metadata().ok()?;
            let modified = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
            let name = path.file_name()?.to_string_lossy().into_owned();
            Some(ClipEntry {
                path,
                name,
                size_mb: meta.len() as f64 / (1024.0 * 1024.0),
                modified,
            })
        })
        .collect();

    clips.sort_by(|a, b| b.modified.cmp(&a.modified));
    clips
}

fn open_in_shell(path: &Path) {
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
