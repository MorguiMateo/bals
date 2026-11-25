use std::path::PathBuf;

use eframe::egui::{self, Align2, Color32, FontId, Rect, Sense, Stroke, StrokeKind, pos2, vec2};

use crate::ui::theme::RetroPalette;
use crate::ui::widgets::{
    draw_dual_trim_timeline, draw_floppy_icon, draw_scanlines, retro_card, retro_pill,
};
use crate::ui::{BalsUiApp, open_in_shell};

/// Pestaña 2 — Archivo de Clips (disquetes) + Monitor CRT con Preview Real & Deck Recortador (Opción C).
pub fn render_clips_tab(
    app: &mut BalsUiApp,
    ui: &mut egui::Ui,
    p: &RetroPalette,
    trim_in_progress: bool,
) {
    let total_w = ui.available_width();
    let left_w = (total_w * 0.39).clamp(240.0, 340.0);
    let right_w = (total_w - left_w - 10.0).max(320.0);
    let avail_h = ui.available_height();

    ui.horizontal_top(|ui| {
        // Columna izquierda: Archivo de clips estilo disquetes retro
        ui.allocate_ui_with_layout(
            vec2(left_w, avail_h),
            egui::Layout::top_down(egui::Align::LEFT),
            |ui| {
                render_clips_archive(app, ui, p);
            },
        );

        ui.add_space(4.0);

        // Columna derecha: Monitor CRT 16:9 + Timeline dual + Botonera Deck
        ui.allocate_ui_with_layout(
            vec2(right_w, avail_h),
            egui::Layout::top_down(egui::Align::LEFT),
            |ui| {
                render_crt_deck(app, ui, p, trim_in_progress);
            },
        );
    });
}

fn render_clips_archive(app: &mut BalsUiApp, ui: &mut egui::Ui, p: &RetroPalette) {
    let badge = format!("{} MP4", app.clips.len());
    retro_card(ui, p, "ARCHIVO DE CLIPS", Some(&badge), |ui| {
        // Controles rápidos: filtro + actualizar + abrir carpeta
        ui.horizontal(|ui| {
            let filter_w = (ui.available_width() - 106.0).max(80.0);
            ui.add(
                egui::TextEdit::singleline(&mut app.clip_filter)
                    .hint_text("Filtrar...")
                    .desired_width(filter_w),
            );
            if retro_pill(ui, p, false, "ACT", vec2(38.0, 22.0))
                .on_hover_text("Actualizar lista de clips")
                .clicked()
            {
                app.refresh_clips();
            }
            if retro_pill(ui, p, false, "DIR", vec2(46.0, 22.0))
                .on_hover_text("Abrir carpeta de clips en el Explorador")
                .clicked()
            {
                let _ = std::fs::create_dir_all(&app.cfg.clips_dir);
                open_in_shell(&app.cfg.clips_dir);
            }
        });

        ui.add_space(6.0);

        let filter_lower = app.clip_filter.trim().to_ascii_lowercase();
        let filtered: Vec<_> = app
            .clips
            .iter()
            .filter(|c| filter_lower.is_empty() || c.name.to_ascii_lowercase().contains(&filter_lower))
            .cloned()
            .collect();

        if filtered.is_empty() {
            ui.add_space(16.0);
            ui.label(
                egui::RichText::new(if app.clips.is_empty() {
                    format!(
                        "SIN CLIPS TODAVÍA.\nPresiona [ {} ] mientras juegas.\n\n{}",
                        app.cfg.save_clip_hotkey,
                        app.cfg.clips_dir.display()
                    )
                } else {
                    "Sin coincidencias para el filtro.".to_string()
                })
                .size(11.5)
                .color(p.crt_muted),
            );
            return;
        }

        let mut clicked_path: Option<PathBuf> = None;
        let scroll_h = (ui.available_height() - 4.0).max(160.0);
        egui::ScrollArea::vertical()
            .max_height(scroll_h)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for clip in &filtered {
                    let is_selected = app.selected_clip.as_ref() == Some(&clip.path);
                    let card_h = 36.0;
                    let (rect, resp) =
                        ui.allocate_exact_size(vec2(ui.available_width(), card_h), Sense::click());
                    let painter = ui.painter();

                    let bg = if is_selected {
                        Color32::from_rgba_unmultiplied(p.accent.r(), p.accent.g(), p.accent.b(), 34)
                    } else if resp.hovered() {
                        p.crt_widget_hover
                    } else {
                        p.crt_recess
                    };
                    let border = if is_selected {
                        p.accent_bright
                    } else if resp.hovered() {
                        p.accent
                    } else {
                        p.crt_widget_hover
                    };

                    if is_selected {
                        let glow = Color32::from_rgba_unmultiplied(
                            p.accent.r(),
                            p.accent.g(),
                            p.accent.b(),
                            28,
                        );
                        painter.rect_filled(rect.expand(1.0), 4.0, glow);
                    }
                    painter.rect_filled(rect, 3.0, bg);
                    painter.rect_stroke(
                        rect,
                        3.0,
                        Stroke::new(if is_selected { 1.5 } else { 1.0 }, border),
                        StrokeKind::Inside,
                    );

                    // Ícono de disquete retro
                    let icon_rect = Rect::from_min_size(
                        pos2(rect.left() + 8.0, rect.center().y - 10.0),
                        vec2(18.0, 20.0),
                    );
                    draw_floppy_icon(painter, icon_rect, p, is_selected);

                    // Nombre truncado + tamaño MB a la derecha
                    let size_txt = format!("{:.1} MB", clip.size_mb);
                    painter.text(
                        pos2(rect.right() - 8.0, rect.center().y),
                        Align2::RIGHT_CENTER,
                        &size_txt,
                        FontId::monospace(11.0),
                        if is_selected { p.accent_bright } else { p.crt_muted },
                    );

                    let max_chars = ((rect.width() - 102.0) / 7.2).max(10.0) as usize;
                    let disp_name = truncate_middle(&clip.name, max_chars);
                    painter.text(
                        pos2(icon_rect.right() + 8.0, rect.center().y),
                        Align2::LEFT_CENTER,
                        disp_name,
                        FontId::monospace(11.5),
                        if is_selected { p.crt_text } else { p.crt_text.linear_multiply(0.9) },
                    );

                    if resp.clicked() {
                        clicked_path = Some(clip.path.clone());
                    }
                    if resp.double_clicked() {
                        open_in_shell(&clip.path);
                    }
                    ui.add_space(4.0);
                }
            });

        if let Some(path) = clicked_path {
            app.select_clip(path);
        }
    });
}

fn render_crt_deck(
    app: &mut BalsUiApp,
    ui: &mut egui::Ui,
    p: &RetroPalette,
    trim_in_progress: bool,
) {
    let meta_badge = app.selected_metadata.map(|m| {
        if m.width > 0 && m.fps > 0.0 {
            format!("{}p · {:.0}fps", m.height, m.fps.round())
        } else {
            format!("{:.1}s", m.duration_secs)
        }
    });

    retro_card(
        ui,
        p,
        "MONITOR CRT & RECORTADOR",
        meta_badge.as_deref(),
        |ui| {
            let Some(sel_path) = app.selected_clip.clone() else {
                ui.add_space(40.0);
                ui.vertical_centered(|ui| {
                    ui.label(
                        egui::RichText::new("[ NO SIGNAL ]\nSelecciona un clip del archivo.")
                            .size(13.0)
                            .color(p.crt_muted),
                    );
                });
                return;
            };

            // 1. Monitor CRT 16:9 con vista previa real del frame decodificado
            let mon_w = ui.available_width();
            let mon_h = (mon_w * 9.0 / 16.0).clamp(155.0, 230.0);
            let (mon_rect, mon_resp) =
                ui.allocate_exact_size(vec2(mon_w, mon_h), Sense::click());
            let painter = ui.painter();

            // Marco oscuro biselado del monitor CRT
            painter.rect_filled(mon_rect, 4.0, p.crt_recess);
            painter.rect_stroke(
                mon_rect,
                4.0,
                Stroke::new(2.0, p.crt_panel_border),
                StrokeKind::Inside,
            );

            let screen_rect = mon_rect.shrink(4.0);
            if let Some(tex) = &app.preview_texture {
                let tex_size = tex.size_vec2();
                let aspect = (tex_size.x / tex_size.y.max(1.0)).max(0.1);
                let fit_w = (screen_rect.height() * aspect).min(screen_rect.width());
                let fit_h = fit_w / aspect;
                let img_rect = Rect::from_center_size(screen_rect.center(), vec2(fit_w, fit_h));
                painter.image(
                    tex.id(),
                    img_rect,
                    Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
                    Color32::WHITE,
                );
            } else {
                let msg = if app.preview_loading {
                    "DECODIFICANDO FRAME CRT..."
                } else {
                    "VISTA PREVIA NO DISPONIBLE"
                };
                painter.text(
                    screen_rect.center(),
                    Align2::CENTER_CENTER,
                    msg,
                    FontId::monospace(12.0),
                    p.crt_muted,
                );
            }

            if app.cfg.crt_scanlines {
                draw_scanlines(painter, screen_rect, p.crt_scanline);
            }

            // Overlays estilo VCR (esquina superior izquierda = timecode, derecha = nombre/resolución)
            let total_dur = app.selected_duration.unwrap_or(0.0);
            let tc_text = format!(
                "{} / {}",
                format_timecode(app.preview_time_secs),
                format_timecode(total_dur)
            );
            draw_vcr_badge(painter, pos2(screen_rect.left() + 8.0, screen_rect.top() + 8.0), &tc_text, p, false);

            let fname = sel_path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            draw_vcr_badge(
                painter,
                pos2(screen_rect.right() - 8.0, screen_rect.top() + 8.0),
                &truncate_middle(&fname, 26),
                p,
                true,
            );

            if mon_resp.double_clicked() {
                open_in_shell(&sel_path);
            }

            ui.add_space(6.0);

            // 2. Línea de tiempo dual interactiva [IN] / [OUT]
            if let Some(total) = app.selected_duration {
                if let Some(seek_t) = draw_dual_trim_timeline(
                    ui,
                    p,
                    total,
                    &mut app.trim_start,
                    &mut app.trim_end,
                    &mut app.trim_drag_handle,
                ) {
                    app.schedule_preview_seek(seek_t);
                }

                // Controles de ajuste fino (-0.5s / +0.5s) y resumen de duración
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new(format!("IN: {:.1}s", app.trim_start))
                            .size(11.5)
                            .color(p.accent),
                    );
                    if retro_pill(ui, p, false, "-.5", vec2(32.0, 20.0)).clicked() {
                        app.trim_start = (app.trim_start - 0.5).max(0.0);
                        app.schedule_preview_seek(app.trim_start);
                    }
                    if retro_pill(ui, p, false, "+.5", vec2(32.0, 20.0)).clicked() {
                        app.trim_start = (app.trim_start + 0.5).min((app.trim_end - 0.5).max(0.0));
                        app.schedule_preview_seek(app.trim_start);
                    }

                    ui.add_space(6.0);
                    ui.label(
                        egui::RichText::new(format!("OUT: {:.1}s", app.trim_end))
                            .size(11.5)
                            .color(p.amber),
                    );
                    if retro_pill(ui, p, false, "-.5", vec2(32.0, 20.0)).clicked() {
                        app.trim_end = (app.trim_end - 0.5).max((app.trim_start + 0.5).min(total));
                        app.schedule_preview_seek(app.trim_end);
                    }
                    if retro_pill(ui, p, false, "+.5", vec2(32.0, 20.0)).clicked() {
                        app.trim_end = (app.trim_end + 0.5).min(total);
                        app.schedule_preview_seek(app.trim_end);
                    }

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(
                            egui::RichText::new(format!(
                                "CORTE: {:.1}s",
                                (app.trim_end - app.trim_start).max(0.0)
                            ))
                            .strong()
                            .size(11.5)
                            .color(p.crt_text),
                        );
                    });
                });
            } else {
                ui.label(
                    egui::RichText::new("No se pudo leer la duración del archivo.")
                        .size(11.5)
                        .color(p.led_red),
                );
            }

            ui.add_space(8.0);

            // 3. Botonera física Deck (Play, Recorte Passthrough, Discord <10MB, <25MB, Copiar, Eliminar)
            ui.add_enabled_ui(!trim_in_progress, |ui| {
                let row_w = ui.available_width();
                ui.horizontal(|ui| {
                    if retro_pill(ui, p, false, "▶ PLAY", vec2(74.0, 30.0))
                        .on_hover_text("Abrir en el reproductor de video de Windows")
                        .clicked()
                    {
                        open_in_shell(&sel_path);
                    }

                    let trim_w = (row_w - 190.0).max(150.0);
                    let trim_lbl = if trim_in_progress {
                        "PROCESANDO..."
                    } else {
                        "RECORTAR SIN PÉRDIDA"
                    };
                    if retro_pill(ui, p, true, trim_lbl, vec2(trim_w, 30.0))
                        .on_hover_text("Corte instantáneo MP4 passthrough sin recodificar")
                        .clicked()
                    {
                        app.start_trim();
                    }

                    if retro_pill(ui, p, false, "RUTA", vec2(50.0, 30.0))
                        .on_hover_text("Copiar ruta completa del clip al portapapeles")
                        .clicked()
                    {
                        ui.ctx().copy_text(sel_path.to_string_lossy().into_owned());
                        app.set_banner("Ruta del clip copiada al portapapeles.", false);
                    }

                    // Botón eliminar con confirmación en 2 clics
                    let confirming = app.confirm_delete.as_ref() == Some(&sel_path);
                    let del_lbl = if confirming { "OK?" } else { "DEL" };
                    let (del_rect, del_resp) = ui.allocate_exact_size(vec2(46.0, 30.0), Sense::click());
                    let del_bg = if confirming {
                        p.danger
                    } else if del_resp.hovered() {
                        Color32::from_rgba_unmultiplied(p.danger.r(), p.danger.g(), p.danger.b(), 150)
                    } else {
                        p.crt_widget_bg
                    };
                    ui.painter().rect_filled(del_rect, 3.0, del_bg);
                    ui.painter().rect_stroke(
                        del_rect,
                        3.0,
                        Stroke::new(1.2, if confirming { p.led_red } else { p.crt_widget_hover }),
                        StrokeKind::Inside,
                    );
                    ui.painter().text(
                        del_rect.center(),
                        Align2::CENTER_CENTER,
                        del_lbl,
                        FontId::monospace(11.5),
                        if confirming { p.danger_text } else { p.crt_text },
                    );
                    if del_resp
                        .on_hover_text(if confirming {
                            "Clic nuevamente para confirmar eliminación permanente"
                        } else {
                            "Eliminar clip seleccionado"
                        })
                        .clicked()
                    {
                        if confirming {
                            if std::fs::remove_file(&sel_path).is_ok() {
                                app.confirm_delete = None;
                                app.set_banner("Clip eliminado permanentemente.", false);
                                app.refresh_clips();
                            }
                        } else {
                            app.confirm_delete = Some(sel_path.clone());
                        }
                    }
                });

                ui.add_space(5.0);

                ui.horizontal(|ui| {
                    let half = ((row_w - 7.0) / 2.0).max(120.0);
                    if retro_pill(ui, p, false, "COMPRIMIR <10 MB (DISCORD)", vec2(half, 26.0))
                        .on_hover_text("Recodifica en GPU para subir directo a Discord gratis")
                        .clicked()
                    {
                        app.start_compress(9.5, "discord");
                    }
                    if retro_pill(ui, p, false, "COMPRIMIR <25 MB", vec2(half, 26.0))
                        .on_hover_text("Recodifica en GPU manteniendo alta calidad bajo 25 MB")
                        .clicked()
                    {
                        app.start_compress(24.0, "25mb");
                    }
                });
            });
        },
    );
}

fn draw_vcr_badge(
    painter: &egui::Painter,
    anchor: egui::Pos2,
    text: &str,
    p: &RetroPalette,
    align_right: bool,
) {
    let w = text.chars().count() as f32 * 6.8 + 12.0;
    let h = 18.0;
    let rect = if align_right {
        Rect::from_min_size(pos2(anchor.x - w, anchor.y), vec2(w, h))
    } else {
        Rect::from_min_size(anchor, vec2(w, h))
    };
    painter.rect_filled(rect, 2.0, Color32::from_rgba_unmultiplied(8, 12, 10, 200));
    painter.rect_stroke(
        rect,
        2.0,
        Stroke::new(1.0, Color32::from_rgba_unmultiplied(p.accent.r(), p.accent.g(), p.accent.b(), 110)),
        StrokeKind::Inside,
    );
    painter.text(
        rect.center(),
        Align2::CENTER_CENTER,
        text,
        FontId::monospace(10.5),
        p.crt_text,
    );
}

fn format_timecode(secs: f64) -> String {
    let clamped = secs.max(0.0);
    let mins = (clamped / 60.0).floor() as u32;
    let rem = clamped - (mins as f64 * 60.0);
    format!("{mins:02}:{rem:04.1}")
}

fn truncate_middle(s: &str, max_chars: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= max_chars || max_chars < 6 {
        return s.to_string();
    }
    let head = (max_chars - 2) / 2;
    let tail = max_chars - 2 - head;
    let left: String = chars[..head].iter().collect();
    let right: String = chars[chars.len() - tail..].iter().collect();
    format!("{left}..{right}")
}
