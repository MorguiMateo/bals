use std::path::PathBuf;

use eframe::egui::{self, Sense, Stroke, StrokeKind, vec2};

use crate::config::{
    AudioCaptureMode, CaptureMode, EncoderPreference, FrameRate, QualityPreset, ReplayDuration,
    Resolution, UiTheme, VideoConfig,
};
use crate::hotkey::parse_hotkey;
use crate::ui::theme::RetroPalette;
use crate::ui::widgets::{draw_led_vu_meter, retro_card, retro_checkbox, retro_pill};
use crate::ui::{BalsUiApp, UpdateUiState, open_in_shell};
use crate::updater::{CURRENT_VERSION, ReleaseInfo};

/// Pestaña 1 — Dashboard estilo Consola Retro OS (Opción A) en grilla modular 2×2.
pub fn render_settings_tab(app: &mut BalsUiApp, ui: &mut egui::Ui, p: &RetroPalette) {
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            // Fila superior: VIDEO (izquierda) + MIXER DE AUDIO (derecha)
            ui.columns(2, |cols| {
                cols[0].vertical(|ui| {
                    render_video_card(app, ui, p);
                });
                cols[1].vertical(|ui| {
                    render_audio_card(app, ui, p);
                });
            });

            ui.add_space(8.0);

            // Fila inferior: MODO & ATAJO GLOBAL (izquierda) + CARPETA, TEMA & SISTEMA (derecha)
            ui.columns(2, |cols| {
                cols[0].vertical(|ui| {
                    render_capture_hotkey_card(app, ui, p);
                });
                cols[1].vertical(|ui| {
                    render_system_card(app, ui, p);
                });
            });
        });
}

fn render_video_card(app: &mut BalsUiApp, ui: &mut egui::Ui, p: &RetroPalette) {
    let hw_badge = app
        .detected_encoders
        .hw_short
        .as_deref()
        .map(|s| format!("H.264 · {s}"))
        .unwrap_or_else(|| "H.264 SW".to_string());

    retro_card(
        ui,
        p,
        "VIDEO // ZERO-COPY GPU",
        Some(&hw_badge),
        |ui| {
            let row_w = ui.available_width();
            let sp = ui.spacing().item_spacing.x;

            // Resolución (3 píldoras)
            ui.label(egui::RichText::new("RESOLUCIÓN").size(10.0).color(p.crt_muted));
            let w3 = ((row_w - sp * 2.0) / 3.0).max(70.0);
            ui.horizontal(|ui| {
                if retro_pill(ui, p, app.cfg.resolution == Resolution::P720, "720p", vec2(w3, 22.0)).clicked() {
                    app.cfg.resolution = Resolution::P720;
                }
                if retro_pill(ui, p, app.cfg.resolution == Resolution::P1080, "1080p", vec2(w3, 22.0)).clicked() {
                    app.cfg.resolution = Resolution::P1080;
                }
                if retro_pill(ui, p, app.cfg.resolution == Resolution::P1440, "2K", vec2(w3, 22.0)).clicked() {
                    app.cfg.resolution = Resolution::P1440;
                }
            });

            // FPS (2 píldoras) + Calidad QVBR (3 píldoras)
            ui.label(egui::RichText::new("FPS Y CALIDAD (QVBR)").size(10.0).color(p.crt_muted));
            let gap = 4.0;
            let usable = row_w - sp * 4.0 - gap;
            let w_fps_btn = ((usable * 0.36) / 2.0).max(52.0);
            let w_qual = ((usable - w_fps_btn * 2.0) / 3.0).max(60.0);
            ui.horizontal(|ui| {
                if retro_pill(ui, p, app.cfg.fps == FrameRate::Fps30, "30 FPS", vec2(w_fps_btn, 22.0)).clicked() {
                    app.cfg.fps = FrameRate::Fps30;
                }
                if retro_pill(ui, p, app.cfg.fps == FrameRate::Fps60, "60 FPS", vec2(w_fps_btn, 22.0)).clicked() {
                    app.cfg.fps = FrameRate::Fps60;
                }
                ui.add_space(gap);
                if retro_pill(
                    ui,
                    p,
                    app.cfg.quality == QualityPreset::Compact,
                    "AHORRO",
                    vec2(w_qual, 22.0),
                )
                .on_hover_text("~45% menos peso y RAM")
                .clicked()
                {
                    app.cfg.quality = QualityPreset::Compact;
                }
                if retro_pill(
                    ui,
                    p,
                    app.cfg.quality == QualityPreset::Balanced,
                    "NORMAL",
                    vec2(w_qual, 22.0),
                )
                .clicked()
                {
                    app.cfg.quality = QualityPreset::Balanced;
                }
                if retro_pill(
                    ui,
                    p,
                    app.cfg.quality == QualityPreset::High,
                    "ALTA",
                    vec2(w_qual, 22.0),
                )
                .clicked()
                {
                    app.cfg.quality = QualityPreset::High;
                }
            });

            // Encoder H.264 (AUTO / GPU HW / CPU SW)
            ui.label(egui::RichText::new("ENCODER H.264").size(10.0).color(p.crt_muted));
            let w_enc_btn = ((row_w - sp * 2.0) / 3.0).max(70.0);

            let gpu_pill_label = app
                .detected_encoders
                .hw_short
                .as_deref()
                .unwrap_or("GPU HW");
            let hw_hover = app
                .detected_encoders
                .hw_name
                .as_deref()
                .map(|n| format!("Codificación H.264 por hardware en tu gráfica: {n}"))
                .unwrap_or_else(|| "No se detectó encoder H.264 por hardware".to_string());
            let sw_hover = app
                .detected_encoders
                .sw_name
                .as_deref()
                .map(|n| format!("Codificación H.264 por CPU (software): {n}"))
                .unwrap_or_else(|| "Codificación H.264 por CPU (Microsoft H.264 MFT)".to_string());

            ui.horizontal(|ui| {
                if retro_pill(
                    ui,
                    p,
                    app.cfg.encoder_pref == EncoderPreference::AutoGpu,
                    "AUTO (GPU)",
                    vec2(w_enc_btn, 22.0),
                )
                .on_hover_text("Usa tu gráfica automáticamente (fallback a CPU si falla)")
                .clicked()
                {
                    app.cfg.encoder_pref = EncoderPreference::AutoGpu;
                }
                ui.add_enabled_ui(app.detected_encoders.hw_name.is_some(), |ui| {
                    if retro_pill(
                        ui,
                        p,
                        app.cfg.encoder_pref == EncoderPreference::HardwareGpu,
                        gpu_pill_label,
                        vec2(w_enc_btn, 22.0),
                    )
                    .on_hover_text(&hw_hover)
                    .clicked()
                    {
                        app.cfg.encoder_pref = EncoderPreference::HardwareGpu;
                    }
                });
                if retro_pill(
                    ui,
                    p,
                    app.cfg.encoder_pref == EncoderPreference::SoftwareCpu,
                    "CPU (SW)",
                    vec2(w_enc_btn, 22.0),
                )
                .on_hover_text(&sw_hover)
                .clicked()
                {
                    app.cfg.encoder_pref = EncoderPreference::SoftwareCpu;
                }
            });

            // Duración del Replay Buffer (4 píldoras)
            ui.label(egui::RichText::new("REPLAY BUFFER EN RAM").size(10.0).color(p.crt_muted));
            let w4 = ((row_w - sp * 3.0) / 4.0).max(52.0);
            ui.horizontal(|ui| {
                for (dur, lbl) in [
                    (ReplayDuration::Sec15, "15s"),
                    (ReplayDuration::Sec30, "30s"),
                    (ReplayDuration::Sec60, "60s"),
                    (ReplayDuration::Sec120, "120s"),
                ] {
                    if retro_pill(ui, p, app.cfg.replay_duration == dur, lbl, vec2(w4, 22.0)).clicked() {
                        app.cfg.replay_duration = dur;
                    }
                }
            });

            ui.add_space(4.0);
            let bitrate_bps = VideoConfig::recommended_bitrate_for(
                app.cfg.resolution,
                app.cfg.fps,
                app.cfg.quality,
            );
            let est_ram_mb = (bitrate_bps as f64 / 8.0) * app.cfg.replay_duration.as_secs() as f64
                / (1024.0 * 1024.0);
            ui.label(
                egui::RichText::new(format!(
                    "{:.1} Mbps (H.264) · ~{:.1} MB RAM · 0 MB/s DISCO",
                    bitrate_bps as f64 / 1_000_000.0,
                    est_ram_mb
                ))
                .size(10.8)
                .color(p.accent),
            );
        },
    );
}

fn render_audio_card(app: &mut BalsUiApp, ui: &mut egui::Ui, p: &RetroPalette) {
    retro_card(
        ui,
        p,
        "MIXER DE AUDIO // WASAPI",
        Some("48kHz AAC"),
        |ui| {
            let row_w = ui.available_width();
            let w2 = ((row_w - 7.0) / 2.0).max(95.0);

            ui.label(egui::RichText::new("FUENTE DE CAPTURA").size(10.5).color(p.crt_muted));
            ui.horizontal(|ui| {
                if retro_pill(
                    ui,
                    p,
                    app.cfg.audio_capture_mode == AudioCaptureMode::FullDesktop,
                    "ESCRITORIO",
                    vec2(w2, 25.0),
                )
                .on_hover_text("Juego + Discord + música del sistema")
                .clicked()
                {
                    app.cfg.audio_capture_mode = AudioCaptureMode::FullDesktop;
                }
                if retro_pill(
                    ui,
                    p,
                    app.cfg.audio_capture_mode == AudioCaptureMode::GameOnly,
                    "SOLO JUEGO",
                    vec2(w2, 25.0),
                )
                .on_hover_text("Excluye Discord y música (ProcessLoopback)")
                .clicked()
                {
                    app.cfg.audio_capture_mode = AudioCaptureMode::GameOnly;
                }
            });

            ui.add_space(6.0);

            // Volumen Juego / Escritorio + VU LED segmentado
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("JUEGO / PC").size(11.5).color(p.crt_text));
                let mut pct = (app.cfg.game_volume * 100.0).round();
                if ui
                    .add(
                        egui::Slider::new(&mut pct, 0.0..=200.0)
                            .suffix("%")
                            .show_value(true),
                    )
                    .changed()
                {
                    app.cfg.game_volume = pct / 100.0;
                }
            });
            draw_led_vu_meter(
                ui,
                p,
                app.desktop_vu_display,
                24,
                14.0,
                "Nivel juego / escritorio",
            );

            ui.add_space(8.0);

            retro_checkbox(ui, p, &mut app.cfg.mic_enabled, "MEZCLAR MICRÓFONO");

            ui.add_enabled_ui(app.cfg.mic_enabled, |ui| {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("MICRÓFONO ").size(11.5).color(p.crt_text));
                    let mut pct = (app.cfg.mic_volume * 100.0).round();
                    if ui
                        .add(
                            egui::Slider::new(&mut pct, 0.0..=200.0)
                                .suffix("%")
                                .show_value(true),
                        )
                        .changed()
                    {
                        app.cfg.mic_volume = pct / 100.0;
                    }
                });
                draw_led_vu_meter(
                    ui,
                    p,
                    if app.cfg.mic_enabled {
                        app.mic_vu_display
                    } else {
                        0.0
                    },
                    24,
                    14.0,
                    "Nivel micrófono",
                );
            });

            ui.add_space(6.0);
            ui.label(
                egui::RichText::new("Ajustar volúmenes al aplicar NO reinicia el Replay Buffer.")
                    .size(10.5)
                    .color(p.crt_muted),
            );
        },
    );
}

fn render_capture_hotkey_card(app: &mut BalsUiApp, ui: &mut egui::Ui, p: &RetroPalette) {
    let mode_badge = match app.cfg.capture_mode {
        CaptureMode::AutoDetectGames => "0% GPU IDLE",
        CaptureMode::ManualMonitor => "MONITOR AUTO",
    };

    retro_card(
        ui,
        p,
        "MODO & ATAJO GLOBAL",
        Some(mode_badge),
        |ui| {
            let row_w = ui.available_width();
            let sp = ui.spacing().item_spacing.x;
            let w_mode = ((row_w - sp) / 2.0).max(110.0);

            ui.horizontal(|ui| {
                if retro_pill(
                    ui,
                    p,
                    app.cfg.capture_mode == CaptureMode::AutoDetectGames,
                    "AUTO-JUEGOS",
                    vec2(w_mode, 25.0),
                )
                .on_hover_text("Inicia automáticamente al abrir un juego y libera 100% GPU en escritorio")
                .clicked()
                {
                    app.cfg.capture_mode = CaptureMode::AutoDetectGames;
                }
                if retro_pill(
                    ui,
                    p,
                    app.cfg.capture_mode == CaptureMode::ManualMonitor,
                    "MONITOR (PANTALLA)",
                    vec2(w_mode, 25.0),
                )
                .on_hover_text("Graba automáticamente toda la pantalla seleccionada en lugar de detectar juegos")
                .clicked()
                {
                    app.cfg.capture_mode = CaptureMode::ManualMonitor;
                    if app.monitors.is_empty() {
                        app.refresh_monitors();
                    }
                }
            });

            if app.cfg.capture_mode == CaptureMode::ManualMonitor {
                ui.add_space(5.0);
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new("PANTALLA A GRABAR (INICIO AUTOMÁTICO)")
                            .size(10.0)
                            .color(p.crt_muted),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if retro_pill(ui, p, false, "ACT", vec2(38.0, 18.0))
                            .on_hover_text("Volver a detectar monitores conectados")
                            .clicked()
                        {
                            app.refresh_monitors();
                        }
                    });
                });

                if app.monitors.is_empty() {
                    ui.label(
                        egui::RichText::new("Usando monitor principal del sistema.")
                            .size(11.0)
                            .color(p.crt_muted),
                    );
                } else {
                    let selected_dev = app.cfg.selected_monitor.as_deref();
                    let per_row = app.monitors.len().clamp(1, 2);
                    let pill_w = ((row_w - sp * (per_row as f32 - 1.0)) / per_row as f32).max(120.0);
                    let mut chosen_dev: Option<String> = None;

                    for chunk in app.monitors.chunks(2) {
                        ui.horizontal(|ui| {
                            for m in chunk {
                                let is_sel = match selected_dev {
                                    Some(dev) => m.device_name.eq_ignore_ascii_case(dev),
                                    None => m.is_primary || m.index == 1,
                                };
                                let hover = if m.is_primary {
                                    format!("Monitor principal ({})", m.device_name)
                                } else {
                                    format!("Monitor secundario ({})", m.device_name)
                                };
                                if retro_pill(ui, p, is_sel, &m.ui_label(), vec2(pill_w, 22.0))
                                    .on_hover_text(hover)
                                    .clicked()
                                {
                                    chosen_dev = Some(m.device_name.clone());
                                }
                            }
                        });
                    }

                    if let Some(dev) = chosen_dev {
                        app.cfg.selected_monitor = Some(dev);
                    }
                }
            }

            ui.add_space(6.0);
            ui.label(egui::RichText::new("TECLA PARA GUARDAR CLIP").size(10.5).color(p.crt_muted));

            // Keycap estilizada + botón interactivo "ASIGNAR TECLA" + edición manual
            ui.horizontal(|ui| {
                let key_display = if app.listening_hotkey {
                    "...".to_string()
                } else {
                    format!("[ {} ]", app.cfg.save_clip_hotkey)
                };
                let (krect, _) = ui.allocate_exact_size(vec2(96.0, 28.0), Sense::hover());
                let painter = ui.painter();
                let kborder = if app.listening_hotkey {
                    p.amber
                } else {
                    p.crt_panel_border
                };
                painter.rect_filled(krect, 3.0, p.crt_recess);
                painter.rect_stroke(krect, 3.0, Stroke::new(1.5, kborder), StrokeKind::Inside);
                painter.text(
                    krect.center(),
                    egui::Align2::CENTER_CENTER,
                    key_display,
                    egui::FontId::monospace(13.0),
                    if app.listening_hotkey { p.amber } else { p.accent_bright },
                );

                let bind_lbl = if app.listening_hotkey {
                    "PRESIONA UNA TECLA... (ESC)"
                } else {
                    "ASIGNAR TECLA"
                };
                let btn_w = (ui.available_width() - 74.0 - sp - 6.0).max(115.0);
                if retro_pill(ui, p, app.listening_hotkey, bind_lbl, vec2(btn_w, 28.0)).clicked() {
                    app.listening_hotkey = !app.listening_hotkey;
                }

                // Entrada directa compacta por si prefiere escribir `Alt+F10`
                let resp = ui.add(
                    egui::TextEdit::singleline(&mut app.cfg.save_clip_hotkey)
                        .desired_width(74.0)
                        .hint_text("\\ / F8"),
                );
                if resp.changed() {
                    app.listening_hotkey = false;
                    app.hotkey_error = parse_hotkey(&app.cfg.save_clip_hotkey)
                        .err()
                        .map(|e| format!("{e}"));
                }
            });

            if let Some(err) = &app.hotkey_error {
                ui.label(egui::RichText::new(err).size(11.0).color(p.led_red));
            }

            ui.add_space(6.0);

            // Checkboxes en 2 columnas limpias
            ui.columns(2, |c| {
                retro_checkbox(&mut c[0], p, &mut app.cfg.capture_cursor, "Capturar cursor");
                retro_checkbox(&mut c[0], p, &mut app.cfg.sound_feedback, "Sonido al guardar");
                retro_checkbox(&mut c[1], p, &mut app.cfg.auto_update, "Auto-actualizar");
                retro_checkbox(
                    &mut c[1],
                    p,
                    &mut app.start_with_windows,
                    "Iniciar con Windows",
                );
            });
        },
    );
}

fn render_system_card(app: &mut BalsUiApp, ui: &mut egui::Ui, p: &RetroPalette) {
    retro_card(
        ui,
        p,
        "CARPETA, TEMA & SISTEMA",
        Some(&format!("v{CURRENT_VERSION}")),
        |ui| {
            let sp = ui.spacing().item_spacing.x;

            // Carpeta de clips
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("CLIPS:").size(11.0).color(p.crt_muted));
                let mut dir_str = app.cfg.clips_dir.to_string_lossy().into_owned();
                let input_w = (ui.available_width() - 66.0 - sp - 6.0).max(120.0);
                if ui
                    .add(egui::TextEdit::singleline(&mut dir_str).desired_width(input_w))
                    .changed()
                {
                    app.cfg.clips_dir = PathBuf::from(dir_str);
                }
                if retro_pill(ui, p, false, "ABRIR", vec2(66.0, 22.0)).clicked() {
                    let _ = std::fs::create_dir_all(&app.cfg.clips_dir);
                    open_in_shell(&app.cfg.clips_dir);
                }
            });

            // Juegos extra en modo ventana
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("EXES: ").size(11.0).color(p.crt_muted));
                let input_w = (ui.available_width() - 6.0).max(120.0);
                ui.add(
                    egui::TextEdit::singleline(&mut app.extra_games_text)
                        .hint_text("ej: javaw.exe, valorant.exe")
                        .desired_width(input_w),
                );
            });

            ui.add_space(4.0);

            // Selector de Tema Retro + toggle Scanlines CRT
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("TEMA:").size(10.5).color(p.crt_muted));
                let crt_w = 56.0;
                let avail = (ui.available_width() - crt_w - sp * 4.0).max(160.0);
                let tw = (avail / 4.0).max(40.0);
                for theme in UiTheme::ALL {
                    if retro_pill(
                        ui,
                        p,
                        app.cfg.ui_theme == theme,
                        theme.short_label(),
                        vec2(tw, 22.0),
                    )
                    .clicked()
                    {
                        app.cfg.ui_theme = theme;
                    }
                }
                retro_checkbox(ui, p, &mut app.cfg.crt_scanlines, "CRT");
            });

            ui.add_space(6.0);

            // Botón principal [ GUARDAR Y APLICAR ] + Auto-updater
            let avail_w = ui.available_width();
            ui.horizontal(|ui| {
                let save_w = ((avail_w - sp) * 0.62).max(165.0);
                if retro_pill(ui, p, true, "[ GUARDAR Y APLICAR ]", vec2(save_w, 30.0))
                    .on_hover_text("Guarda config.toml y aplica al instante (Ctrl+S)")
                    .clicked()
                {
                    app.save_config();
                }

                let upd_w = (avail_w - save_w - sp).max(86.0);
                if retro_pill(ui, p, false, "UPDATES", vec2(upd_w, 30.0))
                    .on_hover_text("Buscar nueva versión en GitHub Releases")
                    .clicked()
                {
                    app.check_for_updates();
                }
            });

            // Estado del auto-updater
            let mut pending_install: Option<ReleaseInfo> = None;
            if let Ok(st) = app.update_state.lock() {
                match &*st {
                    UpdateUiState::Idle => {}
                    UpdateUiState::Checking => {
                        ui.label(
                            egui::RichText::new("Consultando GitHub Releases...")
                                .size(10.5)
                                .color(p.amber),
                        );
                        ui.ctx()
                            .request_repaint_after(std::time::Duration::from_millis(120));
                    }
                    UpdateUiState::UpToDate => {
                        ui.label(
                            egui::RichText::new(format!("OK: Última versión (v{CURRENT_VERSION})"))
                                .size(10.5)
                                .color(p.accent),
                        );
                    }
                    UpdateUiState::Available(info) => {
                        if retro_pill(
                            ui,
                            p,
                            true,
                            &format!("INSTALAR v{}", info.version),
                            vec2(ui.available_width(), 24.0),
                        )
                        .clicked()
                        {
                            pending_install = Some(info.clone());
                        }
                    }
                    UpdateUiState::Downloading => {
                        ui.label(
                            egui::RichText::new("Descargando actualización...")
                                .size(10.5)
                                .color(p.amber),
                        );
                        ui.ctx()
                            .request_repaint_after(std::time::Duration::from_millis(120));
                    }
                    UpdateUiState::Error(e) => {
                        ui.label(
                            egui::RichText::new(format!("ERR: {e}"))
                                .size(10.5)
                                .color(p.led_red),
                        );
                    }
                }
            }
            if let Some(info) = pending_install {
                app.install_update(info);
            }
        },
    );
}
