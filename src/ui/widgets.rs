use eframe::egui::{
    self, Align2, Color32, CornerRadius, FontId, Pos2, Rect, Response, Sense, Stroke, StrokeKind,
    Vec2, pos2, vec2,
};

use crate::ui::theme::RetroPalette;

/// Dibuja una superficie táctil con bisel estilo hardware retro (luz arriba-izquierda, sombra abajo-derecha).
pub fn draw_beveled_box(
    painter: &egui::Painter,
    rect: Rect,
    bg: Color32,
    highlight: Color32,
    shadow: Color32,
    outline: Color32,
    radius: f32,
) {
    painter.rect_filled(rect, radius, bg);

    // Bisel superior e izquierdo (luz)
    let inner = rect.shrink(1.5);
    if inner.width() > 4.0 && inner.height() > 4.0 {
        painter.line_segment(
            [pos2(inner.left(), inner.top()), pos2(inner.right(), inner.top())],
            Stroke::new(1.5, highlight),
        );
        painter.line_segment(
            [pos2(inner.left(), inner.top()), pos2(inner.left(), inner.bottom())],
            Stroke::new(1.5, highlight),
        );
        // Bisel inferior y derecho (sombra)
        painter.line_segment(
            [
                pos2(inner.left(), inner.bottom()),
                pos2(inner.right(), inner.bottom()),
            ],
            Stroke::new(1.5, shadow),
        );
        painter.line_segment(
            [
                pos2(inner.right(), inner.top()),
                pos2(inner.right(), inner.bottom()),
            ],
            Stroke::new(1.5, shadow),
        );
    }

    painter.rect_stroke(rect, radius, Stroke::new(1.5, outline), StrokeKind::Inside);
}

/// Dibuja líneas de barrido horizontales CRT sutiles sin interferir con clics.
pub fn draw_scanlines(painter: &egui::Painter, rect: Rect, color: Color32) {
    let stroke = Stroke::new(1.0, color);
    let mut y = rect.top() + 1.0;
    while y < rect.bottom() {
        painter.line_segment([pos2(rect.left(), y), pos2(rect.right(), y)], stroke);
        y += 3.0;
    }
}

/// Dibuja el marco biselado empotrado de la pantalla CRT oscura.
pub fn draw_crt_recess(painter: &egui::Painter, rect: Rect, p: &RetroPalette) {
    painter.rect_filled(rect, 5.0, p.crt_bg);
    // Sombra interior profunda (efecto vidrio CRT hundido en la carcasa)
    painter.rect_stroke(
        rect,
        5.0,
        Stroke::new(2.5, p.chassis_outline),
        StrokeKind::Inside,
    );
    painter.rect_stroke(
        rect.shrink(2.5),
        4.0,
        Stroke::new(2.0, p.crt_recess),
        StrokeKind::Inside,
    );
}

/// Tarjeta modular estilo Consola Retro OS (Opción A) dentro de la pantalla CRT.
pub fn retro_card(
    ui: &mut egui::Ui,
    p: &RetroPalette,
    title: &str,
    right_badge: Option<&str>,
    add_contents: impl FnOnce(&mut egui::Ui),
) {
    let frame = egui::Frame::new()
        .fill(p.crt_panel_bg)
        .stroke(Stroke::new(1.5, p.crt_panel_border))
        .corner_radius(CornerRadius::same(4))
        .inner_margin(egui::Margin::symmetric(11, 9));

    frame.show(ui, |ui| {
        ui.set_min_width(ui.available_width());
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new(title)
                    .strong()
                    .size(13.0)
                    .color(p.crt_text),
            );
            if let Some(badge) = right_badge {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(
                        egui::RichText::new(badge)
                            .size(11.0)
                            .color(p.amber),
                    );
                });
            }
        });

        // Separador nítido retro
        let (sep_rect, _) =
            ui.allocate_exact_size(vec2(ui.available_width(), 5.0), Sense::hover());
        ui.painter().line_segment(
            [
                pos2(sep_rect.left(), sep_rect.center().y),
                pos2(sep_rect.right(), sep_rect.center().y),
            ],
            Stroke::new(1.0, p.crt_widget_hover),
        );

        ui.add_space(2.0);
        add_contents(ui);
    });
}

/// Botón píldora táctil estilo consola retro (brilla en fósforo cuando está activo).
pub fn retro_pill(
    ui: &mut egui::Ui,
    p: &RetroPalette,
    selected: bool,
    label: &str,
    min_size: Vec2,
) -> Response {
    let (rect, resp) = ui.allocate_exact_size(min_size, Sense::click());
    let painter = ui.painter();

    let bg = if selected {
        p.accent
    } else if resp.hovered() {
        p.crt_widget_hover
    } else {
        p.crt_widget_bg
    };

    let border = if selected {
        p.accent_bright
    } else if resp.hovered() {
        p.accent
    } else {
        p.crt_widget_hover
    };

    let text_color = if selected {
        p.accent_text
    } else {
        p.crt_text
    };

    if selected {
        // Sutil aura fósforo alrededor del botón activo
        let glow = Color32::from_rgba_unmultiplied(p.accent.r(), p.accent.g(), p.accent.b(), 36);
        painter.rect_filled(rect.expand(1.5), 4.0, glow);
    }

    painter.rect_filled(rect, 3.0, bg);
    painter.rect_stroke(rect, 3.0, Stroke::new(1.2, border), StrokeKind::Inside);
    painter.text(
        rect.center(),
        Align2::CENTER_CENTER,
        label,
        FontId::monospace(12.0),
        text_color,
    );

    resp
}

/// Checkbox retro cuadrado con marca nítida estilo consola.
pub fn retro_checkbox(ui: &mut egui::Ui, p: &RetroPalette, value: &mut bool, label: &str) -> Response {
    let mut changed = false;
    let resp = ui
        .horizontal(|ui| {
            let box_size = vec2(15.0, 15.0);
            let (rect, box_resp) = ui.allocate_exact_size(box_size, Sense::click());
            let lbl_resp = ui.add(
                egui::Label::new(egui::RichText::new(label).size(12.0).color(p.crt_text))
                    .sense(Sense::click()),
            );

            if box_resp.clicked() || lbl_resp.clicked() {
                *value = !*value;
                changed = true;
            }

            let hovered = box_resp.hovered() || lbl_resp.hovered();
            let painter = ui.painter();
            let border = if *value || hovered {
                p.accent
            } else {
                p.crt_panel_border
            };
            painter.rect_filled(rect, 2.0, p.crt_recess);
            painter.rect_stroke(rect, 2.0, Stroke::new(1.3, border), StrokeKind::Inside);

            if *value {
                painter.rect_filled(rect.shrink(3.5), 1.0, p.accent);
            }

            box_resp.union(lbl_resp)
        })
        .inner;

    if changed {
        ui.ctx().request_repaint();
    }
    resp
}

/// Botón pestaña físico en el bisel superior de la carcasa CRT (`[L1] AJUSTES`, `[R1] CLIPS`).
pub fn hardware_tab_button(
    ui: &mut egui::Ui,
    p: &RetroPalette,
    active: bool,
    shoulder: &str,
    label: &str,
) -> Response {
    let text = format!("[{shoulder}] {label}");
    let width = (text.chars().count() as f32 * 7.6 + 28.0).max(116.0);
    let (rect, resp) = ui.allocate_exact_size(vec2(width, 30.0), Sense::click());
    let painter = ui.painter();

    if active {
        // Pestaña activa empotrada oscura con LED fósforo encendido
        painter.rect_filled(rect, 3.0, p.crt_bg);
        painter.rect_stroke(rect, 3.0, Stroke::new(1.5, p.chassis_outline), StrokeKind::Inside);
        painter.rect_stroke(
            rect.shrink(1.5),
            2.0,
            Stroke::new(1.0, p.accent),
            StrokeKind::Inside,
        );
        draw_status_led(
            painter,
            pos2(rect.left() + 11.0, rect.center().y),
            3.2,
            p.accent_bright,
            true,
            0.0,
        );
        painter.text(
            pos2(rect.center().x + 5.0, rect.center().y),
            Align2::CENTER_CENTER,
            text,
            FontId::monospace(12.0),
            p.accent_bright,
        );
    } else {
        let bg = if resp.hovered() {
            p.chassis_btn_hover
        } else {
            p.chassis_btn_bg
        };
        draw_beveled_box(
            painter,
            rect,
            bg,
            p.chassis_highlight,
            p.chassis_shadow,
            p.chassis_outline,
            3.0,
        );
        painter.text(
            rect.center(),
            Align2::CENTER_CENTER,
            text,
            FontId::monospace(12.0),
            p.chassis_text,
        );
    }

    resp
}

/// Botón físico táctil del bisel hardware (ej. `[ \ ] CLIP` en rojo o `REC` en crema/oscuro).
pub fn hardware_action_button(
    ui: &mut egui::Ui,
    p: &RetroPalette,
    label: &str,
    fill: Color32,
    text_color: Color32,
    height: f32,
) -> Response {
    let width = (label.chars().count() as f32 * 7.5 + 22.0).max(74.0);
    let (rect, resp) = ui.allocate_exact_size(vec2(width, height), Sense::click());
    let painter = ui.painter();

    let bg = if resp.is_pointer_button_down_on() {
        fill.linear_multiply(0.85)
    } else if resp.hovered() {
        Color32::from_rgb(
            fill.r().saturating_add(16),
            fill.g().saturating_add(16),
            fill.b().saturating_add(16),
        )
    } else {
        fill
    };

    let hi = Color32::from_rgba_unmultiplied(255, 255, 255, 70);
    let sh = Color32::from_rgba_unmultiplied(0, 0, 0, 90);
    draw_beveled_box(painter, rect, bg, hi, sh, p.chassis_outline, 3.0);

    painter.text(
        rect.center(),
        Align2::CENTER_CENTER,
        label,
        FontId::monospace(11.8),
        text_color,
    );

    resp
}

/// Dibuja un LED hardware retro con halo y punto especular.
pub fn draw_status_led(
    painter: &egui::Painter,
    center: Pos2,
    radius: f32,
    color: Color32,
    lit: bool,
    pulse_01: f32,
) {
    if lit {
        let glow_alpha = (55.0 + pulse_01 * 55.0).clamp(30.0, 120.0) as u8;
        let glow = Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), glow_alpha);
        painter.circle_filled(center, radius * 1.9, glow);
        painter.circle_filled(center, radius, color);
        painter.circle_filled(
            pos2(center.x - radius * 0.3, center.y - radius * 0.3),
            radius * 0.35,
            Color32::from_rgba_unmultiplied(255, 255, 255, 190),
        );
    } else {
        painter.circle_filled(center, radius, Color32::from_rgb(55, 58, 56));
    }
    painter.circle_stroke(
        center,
        radius,
        Stroke::new(1.0, Color32::from_rgba_unmultiplied(0, 0, 0, 140)),
    );
}

/// Dibuja las clásicas rejillas de ventilación de un monitor CRT en el bisel inferior.
pub fn draw_vent_grille(ui: &mut egui::Ui, p: &RetroPalette, width: f32, height: f32) {
    let (rect, _) = ui.allocate_exact_size(vec2(width, height), Sense::hover());
    let painter = ui.painter();
    let rows = 4;
    let gap = height / (rows as f32 + 1.0);
    for i in 1..=rows {
        let y = rect.top() + gap * i as f32;
        painter.line_segment(
            [pos2(rect.left(), y), pos2(rect.right(), y)],
            Stroke::new(2.0, p.chassis_shadow),
        );
        painter.line_segment(
            [pos2(rect.left(), y + 1.5), pos2(rect.right(), y + 1.5)],
            Stroke::new(1.0, p.chassis_highlight),
        );
    }
}

/// Medidor VU LED segmentado retro (24 bloques verde → ámbar → rojo) con peak-hold.
pub fn draw_led_vu_meter(
    ui: &mut egui::Ui,
    p: &RetroPalette,
    level: f32,
    peak_hold: f32,
    segments: usize,
    height: f32,
    tooltip: &str,
) {
    let desired = vec2(ui.available_width(), height);
    let (rect, resp) = ui.allocate_exact_size(desired, Sense::hover());
    let painter = ui.painter();

    painter.rect_filled(rect, 2.0, p.crt_recess);
    painter.rect_stroke(
        rect,
        2.0,
        Stroke::new(1.0, p.crt_widget_hover),
        StrokeKind::Inside,
    );

    let inner = rect.shrink(2.0);
    let gap = 2.0;
    let total_gap = gap * (segments.saturating_sub(1)) as f32;
    let seg_w = ((inner.width() - total_gap) / segments as f32).max(2.0);

    let clamped = level.clamp(0.0, 1.0);
    let active_count = (clamped * segments as f32).round() as usize;
    let peak_idx = if peak_hold > 0.03 {
        Some(((peak_hold.clamp(0.0, 1.0) * segments as f32).ceil() as usize).saturating_sub(1))
    } else {
        None
    };

    for i in 0..segments {
        let frac = (i + 1) as f32 / segments as f32;
        let x = inner.left() + i as f32 * (seg_w + gap);
        let seg_rect = Rect::from_min_size(pos2(x, inner.top()), vec2(seg_w, inner.height()));

        let lit_color = if frac > 0.85 {
            p.led_red
        } else if frac > 0.65 {
            p.amber
        } else {
            p.accent
        };

        let is_lit = i < active_count || peak_idx == Some(i);
        let color = if is_lit {
            lit_color
        } else {
            Color32::from_rgba_unmultiplied(lit_color.r(), lit_color.g(), lit_color.b(), 30)
        };
        painter.rect_filled(seg_rect, 1.0, color);
    }

    resp.on_hover_text(format!("{tooltip}: {:.0}%", clamped * 100.0));
}

/// Tira LCD empotrada en el bisel inferior con mini-VU en vivo de Audio y Micrófono.
pub fn draw_mini_bezel_vu(
    ui: &mut egui::Ui,
    p: &RetroPalette,
    game_lvl: f32,
    mic_lvl: f32,
    mic_enabled: bool,
) {
    let width = 236.0;
    let height = 24.0;
    let (rect, _) = ui.allocate_exact_size(vec2(width, height), Sense::hover());
    let painter = ui.painter();

    painter.rect_filled(rect, 3.0, p.crt_recess);
    painter.rect_stroke(
        rect,
        3.0,
        Stroke::new(1.3, p.chassis_outline),
        StrokeKind::Inside,
    );

    let y_mid = rect.center().y;
    painter.text(
        pos2(rect.left() + 8.0, y_mid),
        Align2::LEFT_CENTER,
        "AUD",
        FontId::monospace(10.0),
        p.crt_muted,
    );
    draw_mini_led_strip(
        painter,
        Rect::from_min_size(pos2(rect.left() + 34.0, rect.top() + 6.0), vec2(74.0, 12.0)),
        p,
        game_lvl,
        10,
    );

    painter.text(
        pos2(rect.left() + 120.0, y_mid),
        Align2::LEFT_CENTER,
        if mic_enabled { "MIC" } else { "OFF" },
        FontId::monospace(10.0),
        if mic_enabled { p.crt_muted } else { p.danger },
    );
    draw_mini_led_strip(
        painter,
        Rect::from_min_size(pos2(rect.left() + 148.0, rect.top() + 6.0), vec2(74.0, 12.0)),
        p,
        if mic_enabled { mic_lvl } else { 0.0 },
        10,
    );
}

fn draw_mini_led_strip(
    painter: &egui::Painter,
    rect: Rect,
    p: &RetroPalette,
    level: f32,
    segments: usize,
) {
    let gap = 1.8;
    let seg_w = ((rect.width() - gap * (segments - 1) as f32) / segments as f32).max(2.0);
    let active = (level.clamp(0.0, 1.0) * segments as f32).round() as usize;
    for i in 0..segments {
        let frac = (i + 1) as f32 / segments as f32;
        let c = if frac > 0.85 {
            p.led_red
        } else if frac > 0.65 {
            p.amber
        } else {
            p.accent
        };
        let fill = if i < active {
            c
        } else {
            Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), 32)
        };
        let x = rect.left() + i as f32 * (seg_w + gap);
        painter.rect_filled(
            Rect::from_min_size(pos2(x, rect.top()), vec2(seg_w, rect.height())),
            1.0,
            fill,
        );
    }
}

/// Ícono pixel-art procedural de disquete 3.5" para los clips del archivo.
pub fn draw_floppy_icon(painter: &egui::Painter, rect: Rect, p: &RetroPalette, selected: bool) {
    let body = if selected { p.accent } else { p.crt_widget_hover };
    let border = if selected { p.accent_bright } else { p.crt_muted };
    painter.rect_filled(rect, 2.0, body);
    painter.rect_stroke(rect, 2.0, Stroke::new(1.0, border), StrokeKind::Inside);

    // Obturador metálico superior
    let shutter = Rect::from_min_max(
        pos2(rect.left() + rect.width() * 0.25, rect.top() + 1.5),
        pos2(rect.right() - rect.width() * 0.22, rect.top() + rect.height() * 0.42),
    );
    painter.rect_filled(shutter, 1.0, p.crt_text);
    // Ranura oscura del obturador
    let slot = Rect::from_min_max(
        pos2(shutter.right() - 4.0, shutter.top() + 1.5),
        pos2(shutter.right() - 1.5, shutter.bottom() - 1.5),
    );
    painter.rect_filled(slot, 0.5, p.crt_recess);

    // Etiqueta blanca inferior del disquete
    let label = Rect::from_min_max(
        pos2(rect.left() + 3.0, rect.top() + rect.height() * 0.55),
        pos2(rect.right() - 3.0, rect.bottom() - 2.0),
    );
    painter.rect_filled(label, 1.0, p.crt_text);
    painter.line_segment(
        [
            pos2(label.left() + 2.0, label.center().y),
            pos2(label.right() - 2.0, label.center().y),
        ],
        Stroke::new(1.0, p.crt_recess),
    );
}

/// Barra de recorte interactiva con dos manijas arrastrables `[IN]` y `[OUT]`.
/// Devuelve `Some(timestamp_secs)` cuando el usuario mueve una manija para actualizar el preview CRT.
pub fn draw_dual_trim_timeline(
    ui: &mut egui::Ui,
    p: &RetroPalette,
    total_dur: f64,
    trim_start: &mut f64,
    trim_end: &mut f64,
    drag_mode: &mut Option<TrimDragHandle>,
) -> Option<f64> {
    let mut preview_seek: Option<f64> = None;
    let total = total_dur.max(0.5);
    let min_gap = 0.5_f64.min(total);

    let desired = vec2(ui.available_width(), 44.0);
    let (outer_rect, resp) = ui.allocate_exact_size(desired, Sense::click_and_drag());
    let track_rect = Rect::from_min_max(
        pos2(outer_rect.left() + 8.0, outer_rect.top() + 18.0),
        pos2(outer_rect.right() - 8.0, outer_rect.bottom() - 6.0),
    );

    let frac_start = (*trim_start / total).clamp(0.0, 1.0) as f32;
    let frac_end = (*trim_end / total).clamp(0.0, 1.0) as f32;
    let x_start = track_rect.left() + track_rect.width() * frac_start;
    let x_end = track_rect.left() + track_rect.width() * frac_end;

    // Interacción con el mouse: elegir la manija más cercana al iniciar arrastre o clic.
    if let Some(pointer) = resp.interact_pointer_pos() {
        if resp.drag_started() || resp.clicked() {
            let d_in = (pointer.x - x_start).abs();
            let d_out = (pointer.x - x_end).abs();
            *drag_mode = Some(if d_in <= d_out {
                TrimDragHandle::Start
            } else {
                TrimDragHandle::End
            });
        }
        if resp.dragged() || resp.clicked() {
            let rel = ((pointer.x - track_rect.left()) / track_rect.width()).clamp(0.0, 1.0) as f64;
            let t = rel * total;
            match *drag_mode {
                Some(TrimDragHandle::Start) => {
                    *trim_start = t.clamp(0.0, (*trim_end - min_gap).max(0.0));
                    preview_seek = Some(*trim_start);
                }
                Some(TrimDragHandle::End) => {
                    *trim_end = t.clamp((*trim_start + min_gap).min(total), total);
                    preview_seek = Some(*trim_end);
                }
                None => {}
            }
        }
    }
    if resp.drag_stopped() {
        *drag_mode = None;
    }

    // Recalcular posiciones tras interacción para pintar sin lag de 1 frame.
    let x0 = track_rect.left() + track_rect.width() * (*trim_start / total).clamp(0.0, 1.0) as f32;
    let x1 = track_rect.left() + track_rect.width() * (*trim_end / total).clamp(0.0, 1.0) as f32;

    let painter = ui.painter();

    // Carril base oscuro con marcas de regla retro
    painter.rect_filled(track_rect, 3.0, p.crt_recess);
    painter.rect_stroke(
        track_rect,
        3.0,
        Stroke::new(1.2, p.crt_widget_hover),
        StrokeKind::Inside,
    );
    for tick in 1..10 {
        let tx = track_rect.left() + track_rect.width() * (tick as f32 / 10.0);
        painter.line_segment(
            [pos2(tx, track_rect.top() + 3.0), pos2(tx, track_rect.bottom() - 3.0)],
            Stroke::new(1.0, p.crt_widget_bg),
        );
    }

    // Rango seleccionado en fósforo brillante
    let sel_rect = Rect::from_min_max(
        pos2(x0, track_rect.top() + 1.5),
        pos2(x1.max(x0 + 3.0), track_rect.bottom() - 1.5),
    );
    painter.rect_filled(sel_rect, 2.0, p.accent);

    // Manija [IN]
    let in_active = *drag_mode == Some(TrimDragHandle::Start);
    draw_timeline_handle(painter, p, x0, track_rect, "IN", in_active, true);

    // Manija [OUT]
    let out_active = *drag_mode == Some(TrimDragHandle::End);
    draw_timeline_handle(painter, p, x1, track_rect, "OUT", out_active, false);

    preview_seek
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrimDragHandle {
    Start,
    End,
}

fn draw_timeline_handle(
    painter: &egui::Painter,
    p: &RetroPalette,
    x: f32,
    track: Rect,
    tag: &str,
    active: bool,
    is_left: bool,
) {
    let handle_rect = Rect::from_min_max(
        pos2(x - 4.5, track.top() - 2.0),
        pos2(x + 4.5, track.bottom() + 2.0),
    );
    let fill = if active { p.amber } else { p.crt_text };
    painter.rect_filled(handle_rect, 2.0, fill);
    painter.rect_stroke(
        handle_rect,
        2.0,
        Stroke::new(1.2, p.crt_recess),
        StrokeKind::Inside,
    );

    let label_x = if is_left {
        x.clamp(track.left() + 14.0, track.right() - 24.0)
    } else {
        x.clamp(track.left() + 24.0, track.right() - 16.0)
    };
    painter.text(
        pos2(label_x, track.top() - 10.0),
        Align2::CENTER_CENTER,
        format!("[{tag}]"),
        FontId::monospace(10.0),
        if active { p.amber } else { p.crt_muted },
    );
}
