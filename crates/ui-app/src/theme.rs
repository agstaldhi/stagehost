use eframe::egui::{self, Color32, CornerRadius, Rect, Response, Sense, Stroke, StrokeKind, Vec2};

// Pro-audio studio charcoal palette (neutral base + amber selection accent + semantic signal colors).
pub const BG_CHASSIS: Color32 = Color32::from_rgb(27, 28, 32);
pub const BG_PANEL: Color32 = Color32::from_rgb(35, 37, 42);
pub const BG_RACK_HEADER: Color32 = Color32::from_rgb(45, 48, 55);
pub const BG_ROW_EVEN: Color32 = Color32::from_rgb(31, 33, 38);
pub const BG_ROW_ODD: Color32 = Color32::from_rgb(26, 28, 32);
pub const BG_WELL: Color32 = Color32::from_rgb(19, 20, 24);

pub const BORDER_HAIRLINE: Color32 = Color32::from_rgb(58, 61, 69);
pub const BORDER_SUBTLE: Color32 = Color32::from_rgb(44, 47, 54);

pub const TEXT_PRIMARY: Color32 = Color32::from_rgb(236, 238, 242);
pub const TEXT_SECONDARY: Color32 = Color32::from_rgb(176, 182, 192);
pub const TEXT_MUTED: Color32 = Color32::from_rgb(160, 166, 176);
pub const TEXT_ON_ACCENT: Color32 = Color32::from_rgb(18, 19, 22);

// Single deliberate selection/active accent.
pub const ACCENT_AMBER: Color32 = Color32::from_rgb(229, 152, 36);
pub const ACCENT_AMBER_DIM: Color32 = Color32::from_rgb(64, 48, 22);

// Functional audio telemetry and hazard colors (strictly for meters, LEDs, clip, and delete).
pub const AUDIO_GREEN: Color32 = Color32::from_rgb(66, 184, 114);
pub const AUDIO_YELLOW: Color32 = Color32::from_rgb(229, 168, 36);
pub const AUDIO_RED: Color32 = Color32::from_rgb(242, 110, 110);
pub const AUDIO_RED_ACTIVE: Color32 = Color32::from_rgb(170, 42, 42);
pub const AUDIO_RED_DIM: Color32 = Color32::from_rgb(62, 20, 22);

// Mixer strip and active signal routing tokens.
pub const FL_MINT_GREEN: Color32 = Color32::from_rgb(80, 224, 152);
pub const FL_MINT_DIM: Color32 = Color32::from_rgb(36, 76, 58);
pub const FL_CABLE_GREEN: Color32 = Color32::from_rgb(56, 206, 132);
pub const FL_STRIP_BG: Color32 = Color32::from_rgb(41, 45, 50);
pub const FL_STRIP_BG_ALT: Color32 = Color32::from_rgb(36, 39, 44);
pub const FL_STRIP_WELL: Color32 = Color32::from_rgb(25, 28, 32);
pub const FL_SELECT_BORDER: Color32 = Color32::from_rgb(88, 212, 182);
pub const FL_SLOT_BG: Color32 = Color32::from_rgb(44, 48, 54);
pub const FL_INSPECTOR_BG: Color32 = Color32::from_rgb(36, 40, 46);

pub const FL_STRIP_PRESET_COLORS: &[(&str, Option<[u8; 3]>)] = &[
    ("Default Slate", None),
    ("Bus Crimson", Some([120, 58, 58])),
    ("Vocal Cyan", Some([48, 114, 136])),
    ("Guitar Cobalt", Some([58, 78, 146])),
    ("Keys Amber", Some([134, 96, 44])),
    ("FX Emerald", Some([46, 112, 82])),
    ("Synth Violet", Some([102, 64, 132])),
];

pub fn configure_studio_theme(ctx: &egui::Context) {
    let mut visuals = egui::Visuals::dark();
    visuals.window_fill = BG_PANEL;
    visuals.panel_fill = BG_CHASSIS;
    visuals.extreme_bg_color = BG_WELL;
    visuals.faint_bg_color = BG_ROW_EVEN;
    visuals.override_text_color = Some(TEXT_PRIMARY);
    visuals.weak_text_color = Some(TEXT_SECONDARY);

    let radius = CornerRadius::same(2);
    visuals.window_corner_radius = CornerRadius::same(3);
    visuals.window_stroke = Stroke::new(1.0, BORDER_HAIRLINE);
    visuals.menu_corner_radius = CornerRadius::same(2);

    visuals.widgets.noninteractive.bg_fill = BG_PANEL;
    visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0, BORDER_SUBTLE);
    visuals.widgets.noninteractive.fg_stroke = Stroke::new(1.0, TEXT_PRIMARY);
    visuals.widgets.noninteractive.corner_radius = radius;

    visuals.widgets.inactive.bg_fill = Color32::from_rgb(46, 49, 56);
    visuals.widgets.inactive.bg_stroke = Stroke::new(1.0, BORDER_HAIRLINE);
    visuals.widgets.inactive.fg_stroke = Stroke::new(1.0, TEXT_PRIMARY);
    visuals.widgets.inactive.corner_radius = radius;

    visuals.widgets.hovered.bg_fill = Color32::from_rgb(56, 60, 69);
    visuals.widgets.hovered.bg_stroke = Stroke::new(1.0, ACCENT_AMBER);
    visuals.widgets.hovered.fg_stroke = Stroke::new(1.0, TEXT_PRIMARY);
    visuals.widgets.hovered.corner_radius = radius;

    visuals.widgets.active.bg_fill = Color32::from_rgb(68, 55, 32);
    visuals.widgets.active.bg_stroke = Stroke::new(1.0, ACCENT_AMBER);
    visuals.widgets.active.fg_stroke = Stroke::new(1.0, TEXT_PRIMARY);
    visuals.widgets.active.corner_radius = radius;

    visuals.selection.bg_fill = ACCENT_AMBER_DIM;
    visuals.selection.stroke = Stroke::new(1.0, ACCENT_AMBER);
    ctx.set_visuals(visuals);

    let mut style = (*ctx.global_style()).clone();
    style.animation_time = 0.0;
    style.spacing.item_spacing = Vec2::new(6.0, 4.0);
    style.spacing.button_padding = Vec2::new(8.0, 3.0);
    style.spacing.window_margin = egui::Margin::same(10);
    ctx.set_global_style(style);
}

#[derive(Clone, Copy, Default)]
struct MeterHoldState {
    hold_l: f32,
    hold_r: f32,
    clipped: bool,
}

fn update_meter_hold(
    ui: &egui::Ui,
    id: egui::Id,
    peak_l: f32,
    peak_r: f32,
    clear: bool,
) -> MeterHoldState {
    let dt = ui.input(|i| i.stable_dt.clamp(0.0, 0.1));
    let decay = 0.72_f32.powf(dt * 6.0);
    ui.ctx().data_mut(|d| {
        let mut st = d.get_temp::<MeterHoldState>(id).unwrap_or_default();
        if clear {
            st = MeterHoldState {
                hold_l: peak_l,
                hold_r: peak_r,
                clipped: peak_l >= 0.99 || peak_r >= 0.99,
            };
        } else {
            st.hold_l = (st.hold_l * decay).max(peak_l);
            st.hold_r = (st.hold_r * decay).max(peak_r);
            if peak_l >= 0.99 || peak_r >= 0.99 {
                st.clipped = true;
            }
        }
        d.insert_temp(id, st);
        st
    })
}

/// Draws a flat hardware LED indicator marking a real engine/rack power state.
pub fn draw_led_indicator(ui: &mut egui::Ui, active: bool, active_color: Color32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(10.0), Sense::hover());
    if !ui.is_rect_visible(rect) {
        return;
    }
    let painter = ui.painter();
    let fill = if active {
        active_color
    } else {
        Color32::from_rgb(52, 54, 60)
    };
    painter.circle_filled(rect.center(), 4.0, fill);
    painter.circle_stroke(
        rect.center(),
        4.0,
        Stroke::new(1.0, Color32::from_rgb(16, 17, 20)),
    );
}

/// Hardware-style illuminated console toggle button (e.g. M, S, In, Bypass, Mon).
pub fn draw_hardware_toggle(
    ui: &mut egui::Ui,
    active: &mut bool,
    label: &str,
    active_bg: Color32,
    active_fg: Color32,
    min_width: f32,
) -> Response {
    let btn = if *active {
        egui::Button::new(egui::RichText::new(label).small().strong().color(active_fg))
            .fill(active_bg)
            .stroke(Stroke::new(1.0, active_bg))
            .min_size(Vec2::new(min_width, 20.0))
    } else {
        egui::Button::new(egui::RichText::new(label).small().color(TEXT_SECONDARY))
            .fill(Color32::from_rgb(40, 43, 50))
            .stroke(Stroke::new(1.0, BORDER_HAIRLINE))
            .min_size(Vec2::new(min_width, 20.0))
    };
    let resp = ui.add(btn);
    if resp.clicked() {
        *active = !*active;
    }
    resp
}

/// Compact horizontal stereo dBFS meter with peak hold and click-to-clear clip state.
pub fn draw_stereo_meter(ui: &mut egui::Ui, peak_l: f32, peak_r: f32, width: f32) {
    let desired = Vec2::new(width, 16.0);
    let (rect, resp) = ui.allocate_exact_size(desired, Sense::click());
    if !ui.is_rect_visible(rect) {
        return;
    }

    let hold = update_meter_hold(ui, resp.id, peak_l, peak_r, resp.clicked());
    let painter = ui.painter();
    painter.rect_filled(rect, 1.0, BG_WELL);
    let border_color = if hold.clipped { AUDIO_RED } else { BORDER_HAIRLINE };
    painter.rect_stroke(rect, 1.0, Stroke::new(1.0, border_color), StrokeKind::Inside);

    let inner = rect.shrink(2.0);
    let bar_h = ((inner.height() - 2.0) * 0.5).max(3.0);
    let top_rect = Rect::from_min_size(inner.min, Vec2::new(inner.width(), bar_h));
    let bot_rect = Rect::from_min_size(
        egui::pos2(inner.min.x, inner.min.y + bar_h + 2.0),
        Vec2::new(inner.width(), bar_h),
    );

    paint_horizontal_bar(painter, top_rect, peak_l, hold.hold_l);
    paint_horizontal_bar(painter, bot_rect, peak_r, hold.hold_r);

    resp.on_hover_ui(|ui| {
        ui.label(format!(
            "L: {:.1} dBFS (Peak {:.1}) | R: {:.1} dBFS (Peak {:.1})\nClick meter to clear clip/peak hold",
            audio_core::linear_to_db(peak_l),
            audio_core::linear_to_db(hold.hold_l),
            audio_core::linear_to_db(peak_r),
            audio_core::linear_to_db(hold.hold_r),
        ));
    });
}

fn paint_horizontal_bar(painter: &egui::Painter, rect: Rect, linear_peak: f32, hold_peak: f32) {
    let db = audio_core::linear_to_db(linear_peak);
    let norm = ((db + 60.0) / 60.0).clamp(0.0, 1.0);
    if norm > 0.001 {
        let fill_rect = Rect::from_min_size(rect.min, Vec2::new(rect.width() * norm, rect.height()));
        painter.rect_filled(fill_rect, 0.5, db_meter_color(db));
    }

    let hold_db = audio_core::linear_to_db(hold_peak);
    let hold_norm = ((hold_db + 60.0) / 60.0).clamp(0.0, 1.0);
    if hold_norm > 0.01 {
        let x = (rect.min.x + rect.width() * hold_norm - 2.0).max(rect.min.x);
        let tick = Rect::from_min_size(egui::pos2(x, rect.min.y), Vec2::new(2.0, rect.height()));
        let tick_color = if hold_db >= -1.0 { AUDIO_RED } else { TEXT_PRIMARY };
        painter.rect_filled(tick, 0.0, tick_color);
    }
}

/// Vertical stereo dBFS meter with scale ticks for mixer strips.
pub fn draw_vertical_stereo_meter(
    ui: &mut egui::Ui,
    peak_l: f32,
    peak_r: f32,
    width: f32,
    height: f32,
) {
    let desired = Vec2::new(width, height);
    let (rect, resp) = ui.allocate_exact_size(desired, Sense::click());
    if !ui.is_rect_visible(rect) {
        return;
    }

    let hold = update_meter_hold(ui, resp.id, peak_l, peak_r, resp.clicked());
    let painter = ui.painter();
    painter.rect_filled(rect, 1.0, BG_WELL);
    let border_color = if hold.clipped { AUDIO_RED } else { BORDER_HAIRLINE };
    painter.rect_stroke(rect, 1.0, Stroke::new(1.0, border_color), StrokeKind::Inside);

    let inner = rect.shrink(3.0);
    let gap = 3.0;
    let bar_w = ((inner.width() - gap) * 0.5).max(4.0);
    let left_rect = Rect::from_min_size(inner.min, Vec2::new(bar_w, inner.height()));
    let right_rect = Rect::from_min_size(
        egui::pos2(inner.min.x + bar_w + gap, inner.min.y),
        Vec2::new(bar_w, inner.height()),
    );

    paint_vertical_bar(painter, left_rect, peak_l, hold.hold_l);
    paint_vertical_bar(painter, right_rect, peak_r, hold.hold_r);

    // Subtle dBFS calibration ticks across the center gap (-6, -12, -24, -48 dB).
    for db_mark in [-6.0f32, -12.0, -24.0, -48.0] {
        let norm = (db_mark + 60.0) / 60.0;
        let y = inner.max.y - inner.height() * norm;
        painter.line_segment(
            [egui::pos2(inner.min.x, y), egui::pos2(inner.max.x, y)],
            Stroke::new(1.0, Color32::from_rgba_unmultiplied(255, 255, 255, 22)),
        );
    }

    resp.on_hover_ui(|ui| {
        ui.label(format!(
            "L: {:.1} dBFS (Peak {:.1}) | R: {:.1} dBFS (Peak {:.1})\nClick meter to clear clip/peak hold",
            audio_core::linear_to_db(peak_l),
            audio_core::linear_to_db(hold.hold_l),
            audio_core::linear_to_db(peak_r),
            audio_core::linear_to_db(hold.hold_r),
        ));
    });
}

fn paint_vertical_bar(painter: &egui::Painter, rect: Rect, linear_peak: f32, hold_peak: f32) {
    let db = audio_core::linear_to_db(linear_peak);
    let norm = ((db + 60.0) / 60.0).clamp(0.0, 1.0);
    if norm > 0.001 {
        let h = rect.height() * norm;
        let fill_rect = Rect::from_min_size(
            egui::pos2(rect.min.x, rect.max.y - h),
            Vec2::new(rect.width(), h),
        );
        painter.rect_filled(fill_rect, 0.5, db_meter_color(db));
    }

    let hold_db = audio_core::linear_to_db(hold_peak);
    let hold_norm = ((hold_db + 60.0) / 60.0).clamp(0.0, 1.0);
    if hold_norm > 0.01 {
        let y = (rect.max.y - rect.height() * hold_norm).clamp(rect.min.y, rect.max.y - 2.0);
        let tick = Rect::from_min_size(egui::pos2(rect.min.x, y), Vec2::new(rect.width(), 2.0));
        let tick_color = if hold_db >= -1.0 { AUDIO_RED } else { TEXT_PRIMARY };
        painter.rect_filled(tick, 0.0, tick_color);
    }
}

/// Real-time chromatic tuner deviation bar (-50 cents to +50 cents).
pub fn draw_tuner_deviation_bar(ui: &mut egui::Ui, cents: f32, active: bool) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(76.0, 14.0), Sense::hover());
    if !ui.is_rect_visible(rect) {
        return;
    }
    let painter = ui.painter();
    painter.rect_filled(rect, 1.0, BG_WELL);
    painter.rect_stroke(rect, 1.0, Stroke::new(1.0, BORDER_HAIRLINE), StrokeKind::Inside);

    let center_x = rect.center().x;
    painter.line_segment(
        [
            egui::pos2(center_x, rect.min.y + 1.0),
            egui::pos2(center_x, rect.max.y - 1.0),
        ],
        Stroke::new(1.0, BORDER_HAIRLINE),
    );

    if active {
        let norm = (cents / 50.0).clamp(-1.0, 1.0);
        let x = center_x + norm * (rect.width() * 0.5 - 4.0);
        let needle_color = if cents.abs() <= 5.0 {
            AUDIO_GREEN
        } else {
            ACCENT_AMBER
        };
        let needle = Rect::from_center_size(egui::pos2(x, rect.center().y), Vec2::new(4.0, rect.height() - 4.0));
        painter.rect_filled(needle, 0.5, needle_color);
    }
}

fn db_meter_color(db: f32) -> Color32 {
    if db >= -1.0 {
        AUDIO_RED
    } else if db >= -9.0 {
        AUDIO_YELLOW
    } else {
        AUDIO_GREEN
    }
}

/// Blends an optional RGB strip tint softly into a dark studio base color.
pub fn blend_strip_tint(base: Color32, tint: Option<[u8; 3]>, amount: f32) -> Color32 {
    let Some([tr, tg, tb]) = tint else {
        return base;
    };
    let a = amount.clamp(0.0, 1.0);
    let mix = |b: u8, t: u8| -> u8 { ((b as f32) * (1.0 - a) + (t as f32) * a).round() as u8 };
    Color32::from_rgb(mix(base.r(), tr), mix(base.g(), tg), mix(base.b(), tb))
}

/// Draws a circular rotary knob (Pan, Wet/Dry Mix, or Send Level) with drag & double-click reset.
#[allow(clippy::too_many_arguments)]
pub fn draw_rotary_knob(
    ui: &mut egui::Ui,
    id: egui::Id,
    value: &mut f32,
    range: std::ops::RangeInclusive<f32>,
    default_value: f32,
    diameter: f32,
    arc_color: Color32,
    bipolar: bool,
) -> Response {
    let (rect, mut resp) = ui.allocate_exact_size(Vec2::splat(diameter), Sense::click_and_drag());
    if resp.dragged() {
        let delta_y = -resp.drag_delta().y;
        let span = (*range.end() - *range.start()).max(1e-5);
        let fine = if ui.input(|i| i.modifiers.shift) {
            0.25
        } else {
            1.0
        };
        *value = (*value + (delta_y / 110.0) * span * fine).clamp(*range.start(), *range.end());
        resp.mark_changed();
    }
    if resp.double_clicked() {
        *value = default_value.clamp(*range.start(), *range.end());
        resp.mark_changed();
    }

    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        let center = rect.center();
        let radius = diameter * 0.5 - 2.0;
        let start_angle = 135.0_f32.to_radians();
        let sweep_total = 270.0_f32.to_radians();
        let span = (*range.end() - *range.start()).max(1e-5);
        let norm = ((*value - *range.start()) / span).clamp(0.0, 1.0);
        let angle = start_angle + norm * sweep_total;

        // Outer track arc.
        paint_arc(
            painter,
            center,
            radius,
            start_angle,
            start_angle + sweep_total,
            Stroke::new(2.0, Color32::from_rgb(24, 26, 30)),
        );

        // Active value arc (either from center for bipolar Pan, or from start for unipolar).
        let (arc_from, arc_to) = if bipolar {
            let mid_angle = start_angle + 0.5 * sweep_total;
            if angle >= mid_angle {
                (mid_angle, angle)
            } else {
                (angle, mid_angle)
            }
        } else {
            (start_angle, angle)
        };
        if (arc_to - arc_from).abs() > 0.01 {
            paint_arc(painter, center, radius, arc_from, arc_to, Stroke::new(2.2, arc_color));
        }

        // Knob cap & pointer line.
        let cap_r = (radius - 2.2).max(3.5);
        let cap_fill = if resp.hovered() || resp.dragged() {
            Color32::from_rgb(56, 61, 69)
        } else {
            Color32::from_rgb(42, 46, 52)
        };
        painter.circle_filled(center, cap_r, cap_fill);
        painter.circle_stroke(center, cap_r, Stroke::new(1.0, Color32::from_rgb(20, 22, 26)));

        let dir = Vec2::new(angle.cos(), angle.sin());
        painter.line_segment(
            [center + dir * (cap_r * 0.25), center + dir * (cap_r - 1.0)],
            Stroke::new(1.6, TEXT_PRIMARY),
        );
    }

    let _ = id;
    resp
}

fn paint_arc(
    painter: &egui::Painter,
    center: egui::Pos2,
    radius: f32,
    from_angle: f32,
    to_angle: f32,
    stroke: Stroke,
) {
    const STEPS: usize = 16;
    let mut prev = center + Vec2::new(from_angle.cos(), from_angle.sin()) * radius;
    for i in 1..=STEPS {
        let t = i as f32 / STEPS as f32;
        let a = from_angle + (to_angle - from_angle) * t;
        let next = center + Vec2::new(a.cos(), a.sin()) * radius;
        painter.line_segment([prev, next], stroke);
        prev = next;
    }
}

/// Vertical mixer fader with dB scale, beveled thumb, and optional stereo meter.
#[allow(clippy::too_many_arguments)]
pub fn draw_vertical_fader(
    ui: &mut egui::Ui,
    value_db: &mut f32,
    range: std::ops::RangeInclusive<f32>,
    default_db: f32,
    width: f32,
    height: f32,
    selected: bool,
    meter_peaks: Option<(f32, f32)>,
) -> Response {
    let (rect, mut resp) = ui.allocate_exact_size(Vec2::new(width, height), Sense::click_and_drag());
    let min_db = *range.start();
    let max_db = *range.end();
    let span = (max_db - min_db).max(1e-4);
    let pad_y = 9.0;
    let travel_top = rect.min.y + pad_y;
    let travel_bot = rect.max.y - pad_y;
    let travel_h = (travel_bot - travel_top).max(12.0);

    if resp.dragged() {
        let dy = -resp.drag_delta().y;
        let fine = if ui.input(|i| i.modifiers.shift) {
            0.25
        } else {
            1.0
        };
        *value_db = (*value_db + (dy / travel_h) * span * fine).clamp(min_db, max_db);
        resp.mark_changed();
    } else if resp.clicked() {
        if let Some(pos) = resp.interact_pointer_pos() {
            let norm = ((travel_bot - pos.y) / travel_h).clamp(0.0, 1.0);
            *value_db = min_db + norm * span;
            resp.mark_changed();
        }
    }
    if resp.double_clicked() {
        *value_db = default_db.clamp(min_db, max_db);
        resp.mark_changed();
    }

    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        let center_x = rect.center().x;

        // Optional background stereo meter column slightly right of fader slot.
        if let Some((peak_l, peak_r)) = meter_peaks {
            let meter_rect = Rect::from_min_max(
                egui::pos2(center_x + 7.0, travel_top),
                egui::pos2(rect.max.x - 3.0, travel_bot),
            );
            if meter_rect.width() >= 4.0 {
                painter.rect_filled(meter_rect, 1.0, BG_WELL);
                let hold = update_meter_hold(ui, resp.id.with("fader_meter"), peak_l, peak_r, false);
                let half_w = ((meter_rect.width() - 1.0) * 0.5).max(1.5);
                let l_rect = Rect::from_min_size(meter_rect.min, Vec2::new(half_w, meter_rect.height()));
                let r_rect = Rect::from_min_size(
                    egui::pos2(meter_rect.min.x + half_w + 1.0, meter_rect.min.y),
                    Vec2::new(half_w, meter_rect.height()),
                );
                paint_vertical_bar(painter, l_rect, peak_l, hold.hold_l);
                paint_vertical_bar(painter, r_rect, peak_r, hold.hold_r);
            }
        }

        // Recessed vertical fader slot.
        let slot_x = if meter_peaks.is_some() {
            center_x - 5.0
        } else {
            center_x
        };
        let slot_rect = Rect::from_center_size(
            egui::pos2(slot_x, rect.center().y),
            Vec2::new(4.0, travel_h + 4.0),
        );
        painter.rect_filled(slot_rect, 1.5, Color32::from_rgb(16, 18, 21));
        painter.rect_stroke(
            slot_rect,
            1.5,
            Stroke::new(1.0, Color32::from_rgb(52, 56, 64)),
            StrokeKind::Inside,
        );

        // 0 dB unity tick mark.
        if (min_db..=max_db).contains(&0.0) {
            let zero_norm = (0.0 - min_db) / span;
            let zero_y = travel_bot - zero_norm * travel_h;
            painter.line_segment(
                [egui::pos2(slot_x - 8.0, zero_y), egui::pos2(slot_x + 8.0, zero_y)],
                Stroke::new(1.0, Color32::from_rgb(95, 102, 112)),
            );
        }

        // Highlight fader track below thumb when selected.
        let val_norm = ((*value_db - min_db) / span).clamp(0.0, 1.0);
        let thumb_y = travel_bot - val_norm * travel_h;
        if selected {
            let active_track = Rect::from_min_max(
                egui::pos2(slot_x - 1.5, thumb_y),
                egui::pos2(slot_x + 1.5, travel_bot),
            );
            painter.rect_filled(active_track, 1.0, FL_MINT_GREEN);
        }

        // Beveled fader thumb.
        let thumb_w = 18.0;
        let thumb_h = 13.0;
        let thumb_rect = Rect::from_center_size(egui::pos2(slot_x, thumb_y), Vec2::new(thumb_w, thumb_h));
        let thumb_fill = if selected {
            FL_MINT_GREEN
        } else if resp.hovered() || resp.dragged() {
            Color32::from_rgb(112, 118, 128)
        } else {
            Color32::from_rgb(82, 87, 96)
        };
        let thumb_border = if selected {
            Color32::from_rgb(32, 92, 66)
        } else {
            Color32::from_rgb(22, 24, 28)
        };
        painter.rect_filled(thumb_rect, 2.0, thumb_fill);
        painter.rect_stroke(thumb_rect, 2.0, Stroke::new(1.0, thumb_border), StrokeKind::Inside);
        painter.line_segment(
            [
                egui::pos2(thumb_rect.min.x + 3.0, thumb_rect.center().y),
                egui::pos2(thumb_rect.max.x - 3.0, thumb_rect.center().y),
            ],
            Stroke::new(1.2, Color32::from_rgb(20, 24, 28)),
        );
    }

    resp
}

/// Draws vertical dB calibration ruler on the left edge of the mixer.
pub fn draw_db_scale_ruler(ui: &mut egui::Ui, width: f32, height: f32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, height), Sense::hover());
    if !ui.is_rect_visible(rect) {
        return;
    }
    let painter = ui.painter();
    painter.rect_filled(rect, 0.0, FL_STRIP_WELL);
    painter.rect_stroke(
        rect,
        0.0,
        Stroke::new(1.0, BORDER_SUBTLE),
        StrokeKind::Inside,
    );

    let font_id = egui::FontId::monospace(8.5);
    for (db_mark, label) in [
        (0.0_f32, "0"),
        (-6.0, "6"),
        (-12.0, "12"),
        (-18.0, "18"),
        (-24.0, "24"),
        (-30.0, "30"),
        (-42.0, "42"),
        (-54.0, "54"),
    ] {
        let norm = ((db_mark + 60.0) / 60.0).clamp(0.0, 1.0);
        let y = rect.max.y - 4.0 - (rect.height() - 8.0) * norm;
        painter.line_segment(
            [egui::pos2(rect.max.x - 5.0, y), egui::pos2(rect.max.x - 1.0, y)],
            Stroke::new(1.0, Color32::from_rgb(76, 132, 114)),
        );
        painter.text(
            egui::pos2(rect.max.x - 7.0, y),
            egui::Align2::RIGHT_CENTER,
            label,
            font_id.clone(),
            Color32::from_rgb(86, 168, 134),
        );
    }
}

/// Draws vertical rotated strip label reading upward along the mixer strip.
pub fn draw_rotated_vertical_text(
    ui: &egui::Ui,
    rect: Rect,
    text: &str,
    color: Color32,
    font_size: f32,
) {
    if !ui.is_rect_visible(rect) {
        return;
    }
    let font_id = egui::FontId::proportional(font_size);
    let galley = ui.painter().layout_no_wrap(text.to_string(), font_id, color);
    // Rotate -90 deg so text reads upward; anchor bottom-left near bottom of rect.
    let text_w = galley.size().x.min(rect.height() - 6.0);
    let pos = egui::pos2(
        rect.center().x - galley.size().y * 0.5,
        (rect.max.y - 4.0).max(rect.min.y + text_w + 4.0),
    );
    let clipped = ui.painter().with_clip_rect(rect);
    clipped.add(
        egui::epaint::TextShape::new(pos, galley, color)
            .with_angle(-std::f32::consts::FRAC_PI_2),
    );
}

/// Draws a Bezier routing cable between two bottom strip jacks.
pub fn draw_routing_cable(
    painter: &egui::Painter,
    from: egui::Pos2,
    to: egui::Pos2,
    active: bool,
    gutter_drop: f32,
) {
    let drop = gutter_drop.clamp(12.0, 42.0);
    let cp1 = egui::pos2(from.x, from.y + drop);
    let cp2 = egui::pos2(to.x, to.y + drop);
    let points = [from, cp1, cp2, to];

    let stroke = if active {
        Stroke::new(1.8, FL_CABLE_GREEN)
    } else {
        Stroke::new(1.2, Color32::from_rgb(76, 98, 88))
    };
    painter.add(egui::epaint::CubicBezierShape::from_points_stroke(
        points,
        false,
        Color32::TRANSPARENT,
        stroke,
    ));
}
