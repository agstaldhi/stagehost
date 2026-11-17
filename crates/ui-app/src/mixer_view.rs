use std::collections::HashSet;

use eframe::egui::{self, Color32, CornerRadius, Pos2, Rect, RichText, Sense, Stroke, StrokeKind, Vec2};
use session_core::{
    InputChannelMode, PluginKind, PluginSlotConfig, RackConfig, RackSendConfig, SidechainConfig,
    SidechainSourceKind,
};
use uuid::Uuid;

use crate::app::StageHostApp;
use crate::file_dialog;
use crate::panels::{
    add_plugin_viewport_id, draw_slot_parameter_drawer, draw_slot_sidechain_drawer,
    format_delay_ms, input_mode_label, next_input_mode, open_keybindings_window,
    open_options_window, KeybindTarget, LatencyDisplayCtx, SidechainDisplayCtx,
};
use crate::theme::{
    self, ACCENT_AMBER, AUDIO_RED, AUDIO_RED_ACTIVE, AUDIO_YELLOW, BG_PANEL, BG_WELL,
    BORDER_HAIRLINE, BORDER_SUBTLE, FL_CABLE_GREEN, FL_MINT_DIM, FL_MINT_GREEN, FL_SELECT_BORDER,
    FL_SLOT_BG, FL_STRIP_BG, FL_STRIP_BG_ALT, FL_STRIP_PRESET_COLORS, FL_STRIP_WELL, TEXT_MUTED,
    TEXT_ON_ACCENT, TEXT_PRIMARY, TEXT_SECONDARY,
};

// Identical dimensions for IN, Master (M), and all Rack strips.
const STRIP_WIDTH: f32 = 56.0;
const RULER_WIDTH: f32 = 24.0;
const CABLE_GUTTER_HEIGHT: f32 = 34.0;

// Shared vertical row heights so IN, M, and Rack 1..N align pixel-for-pixel.
const ROW_HDR_H: f32 = 18.0;
const ROW_LED_H: f32 = 16.0;
const ROW_CTRL_H: f32 = 18.0;
const ROW_DB_H: f32 = 15.0;
const ROW_STATUS_H: f32 = 14.0;
const ROW_SOCKET_H: f32 = 26.0;

#[derive(Default)]
struct StripMutations {
    select_rack: Option<Uuid>,
    toggle_multi_select: Option<Uuid>,
    clear_multi_select: bool,
    deselect_all: bool,
    rename_rack: Option<Uuid>,
    param_dirty: bool,
    order_dirty: bool,
    add_rack: Option<bool>, // Some(receive_input)
    delete_rack: Option<Uuid>,
    duplicate_rack: Option<Uuid>,
    move_rack: Option<(usize, isize)>,
    reorder_rack: Option<(Uuid, usize)>,
    bind_rack: Option<Uuid>,
    bind_multi_rack: Option<Vec<Uuid>>,
    group_selected_to_new_bus: bool,
    group_selected_to_rack: Option<Uuid>,
    link_selected_faders: bool,
    ungroup_rack: Option<Uuid>,
    dissolve_group: Option<Uuid>,
    gain_delta: Option<(Uuid, Option<Uuid>, f32)>,
    batch_color_selected: Option<Option<[u8; 3]>>,
    batch_mute_selected: Option<bool>,
    batch_enable_selected: Option<bool>,
    toggle_send: Option<(Uuid, Uuid)>,
    remove_send: Option<(Uuid, Uuid)>,
    add_send: Option<(Uuid, Uuid)>,
    toggle_master_route: Option<Uuid>,
    set_sidechain_link: Option<(Uuid, Uuid, Option<Uuid>, bool)>, // (src_rack, dst_rack, slot_id, pre_fader)
    remove_sidechain_link: Option<(Uuid, Uuid)>,                   // (src_rack, dst_rack)
    open_settings: bool,
    save_all_settings: bool,
    load_all_settings: bool,
}

/// Prunes stale rack IDs while allowing explicit empty selection (`None`).
pub fn ensure_valid_selected_rack(app: &mut StageHostApp) -> Option<Uuid> {
    app.selected_rack_ids
        .retain(|id| app.session.racks.iter().any(|r| r.id == *id));
    let valid = app
        .selected_rack_id
        .filter(|id| app.session.racks.iter().any(|r| r.id == *id));
    app.selected_rack_id = valid;
    if let Some(primary) = valid {
        if !app.selected_rack_ids.contains(&primary) {
            app.selected_rack_ids.push(primary);
        }
    } else {
        app.selected_rack_ids.clear();
    }
    valid
}

/// Renders the Mixer Rack strips and audio/sidechain Bezier cables.
pub fn draw_center_mixer_strips(app: &mut StageHostApp, ui: &mut egui::Ui) {
    let selected_id = ensure_valid_selected_rack(app);
    draw_mixer_toolbar(app, ui);
    ui.add_space(4.0);

    if app.session.racks.is_empty() {
        draw_empty_mixer_placeholder(app, ui);
        return;
    }

    let total_avail_h = ui.available_height().max(320.0);
    let strip_h = (total_avail_h - CABLE_GUTTER_HEIGHT - 8.0).max(260.0);
    let meter_zone_h = (strip_h * 0.25).clamp(84.0, 128.0);
    let fixed_rows_h =
        ROW_HDR_H + meter_zone_h + ROW_LED_H + ROW_CTRL_H + ROW_DB_H + ROW_STATUS_H + ROW_SOCKET_H + 30.0;
    let fader_h = (strip_h - fixed_rows_h).max(96.0);

    let mut muts = StripMutations::default();
    let mut master_jack_pos: Option<Pos2> = None;
    let mut rack_jack_positions: Vec<(Uuid, Pos2, Rect)> = Vec::new();
    let mut hovered_target: Option<Option<Uuid>> = None; // Some(None) = Master, Some(Some(id)) = Rack
    let mut hovered_rack_drop_idx: Option<usize> = None;

    let selected_sends: Vec<RackSendConfig> = selected_id
        .and_then(|sid| app.session.racks.iter().find(|r| r.id == sid))
        .map(|r| r.sends.clone())
        .unwrap_or_default();
    let selected_to_master = selected_id
        .and_then(|sid| app.session.racks.iter().find(|r| r.id == sid))
        .is_some_and(|r| r.route_to_master);

    // Pre-collect which target racks have slots sidechained to `selected_id`
    let selected_sc_targets: Vec<(Uuid, Uuid, String, f32, bool)> = selected_id
        .map(|sid| collect_outgoing_sidechains(&app.session.racks, sid))
        .unwrap_or_default();

    let outer_rect = ui.available_rect_before_wrap();
    let cable_clip_rect = Rect::from_min_max(
        egui::pos2(outer_rect.min.x + RULER_WIDTH + STRIP_WIDTH + 2.0, outer_rect.min.y),
        outer_rect.max,
    );

    let mut rename_anchor: Option<Pos2> = None;

    ui.horizontal_top(|ui| {
        ui.spacing_mut().item_spacing.x = 2.0;

        // 1) Left dB Scale Ruler aligned with the meter zone.
        ui.vertical(|ui| {
            ui.add_space(ROW_HDR_H + 5.0);
            theme::draw_db_scale_ruler(ui, RULER_WIDTH, meter_zone_h);
        });

        // 2) Live Input Strip (IN) - identical width & height to Rack strips.
        draw_input_mixer_strip(app, strip_h, meter_zone_h, fader_h, &mut muts, ui);

        // 3) Master Strip (M) - identical width & height to Rack strips.
        let (m_jack, m_hovered) = draw_master_mixer_strip(
            app,
            selected_id,
            selected_to_master,
            strip_h,
            meter_zone_h,
            fader_h,
            &mut muts,
            ui,
        );
        master_jack_pos = Some(m_jack);
        if m_hovered {
            hovered_target = Some(None);
        }

        // Divider bar between Master and Rack Inserts.
        let (sep_rect, _) = ui.allocate_exact_size(Vec2::new(3.0, strip_h), Sense::hover());
        ui.painter().rect_filled(sep_rect, 0.0, Color32::from_rgb(16, 18, 21));

        // 4) Scrollable Rack Strips (1..N) + Empty Area Right-Click Target.
        egui::ScrollArea::horizontal()
            .id_salt("fl_mixer_strips_scroll")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.horizontal_top(|ui| {
                    ui.spacing_mut().item_spacing.x = 2.0;
                    let rack_count = app.session.racks.len();
                    let sr = if app.telemetry.sample_rate > 0 {
                        app.telemetry.sample_rate
                    } else {
                        app.selected_sample_rate.max(8_000)
                    };
                    let audio_running = app.audio_engine.is_running();

                    // Pre-gather sidechain capability info per rack without borrowing racks mutably twice.
                    let rack_sc_slots: Vec<Vec<(Uuid, String, Option<SidechainConfig>)>> = app
                        .session
                        .racks
                        .iter()
                        .map(|r| {
                            r.slots
                                .iter()
                                .filter(|s| {
                                    StageHostApp::resolve_slot_sidechain_support(
                                        &app.slot_sidechain_caps,
                                        &app.scanned_plugins,
                                        s,
                                    ) != Some(false)
                                })
                                .map(|s| (s.id, s.name.clone(), s.sidechain.clone()))
                                .collect()
                        })
                        .collect();

                    let all_rack_names: Vec<(Uuid, String)> = app
                        .session
                        .racks
                        .iter()
                        .map(|r| (r.id, r.name.clone()))
                        .collect();
                    let selected_ids_snapshot = app.selected_rack_ids.clone();

                    for r_idx in 0..rack_count {
                        let rack_id = app.session.racks[r_idx].id;
                        let meters = app.rack_meters.get(&rack_id).copied().unwrap_or((0.0, 0.0));
                        let rack_latency: u32 = if audio_running
                            && app.session.racks[r_idx].enabled
                            && !app.session.racks[r_idx].muted
                        {
                            app.session.racks[r_idx]
                                .slots
                                .iter()
                                .filter(|s| s.enabled && !s.bypassed)
                                .filter_map(|s| app.slot_latencies.get(&s.id).copied())
                                .fold(0u32, u32::saturating_add)
                        } else {
                            0
                        };

                        let is_selected = selected_id == Some(rack_id);
                        let is_multi_selected = selected_ids_snapshot.contains(&rack_id);
                        let send_from_selected = if !is_selected {
                            selected_sends.iter().find(|s| s.target_rack_id == rack_id).cloned()
                        } else {
                            None
                        };
                        let sc_from_selected = if !is_selected {
                            selected_sc_targets
                                .iter()
                                .find(|(tid, _, _, _, _)| *tid == rack_id)
                                .map(|(_, slot_id, slot_name, gain_db, pre_fader)| {
                                    (*slot_id, slot_name.clone(), *gain_db, *pre_fader)
                                })
                        } else {
                            None
                        };

                        let mut edited_send_level: Option<(f32, bool)> = None;
                        let mut edited_sc_gain: Option<(Uuid, f32)> = None;

                        let res = draw_single_rack_strip(
                            &mut app.session.racks[r_idx],
                            r_idx,
                            rack_count,
                            is_selected,
                            is_multi_selected,
                            &selected_ids_snapshot,
                            &all_rack_names,
                            selected_id,
                            send_from_selected.as_ref(),
                            sc_from_selected.as_ref(),
                            &rack_sc_slots[r_idx],
                            &mut edited_send_level,
                            &mut edited_sc_gain,
                            meters,
                            rack_latency,
                            sr,
                            audio_running,
                            strip_h,
                            meter_zone_h,
                            fader_h,
                            &mut app.cable_drag_source,
                            &mut app.rack_drag_source,
                            &mut muts,
                            ui,
                        );

                        if app.renaming_rack_id == Some(rack_id) {
                            rename_anchor = Some(res.strip_rect.left_top());
                        }

                        if let Some((new_level_db, true)) = edited_send_level {
                            if let Some(sid) = selected_id {
                                if let Some(src_rack) =
                                    app.session.racks.iter_mut().find(|r| r.id == sid)
                                {
                                    if let Some(send) = src_rack
                                        .sends
                                        .iter_mut()
                                        .find(|s| s.target_rack_id == rack_id)
                                    {
                                        send.level_db = new_level_db;
                                        muts.param_dirty = true;
                                    }
                                }
                            }
                        }

                        if let Some((slot_id, new_sc_db)) = edited_sc_gain {
                            if let Some(slot) = app.session.racks[r_idx]
                                .slots
                                .iter_mut()
                                .find(|s| s.id == slot_id)
                            {
                                if let Some(ref mut sc) = slot.sidechain {
                                    sc.gain_db = new_sc_db;
                                    muts.param_dirty = true;
                                }
                            }
                        }

                        if res.hovered_jack {
                            hovered_target = Some(Some(rack_id));
                        }
                        if app.rack_drag_source.is_some() && res.strip_hovered {
                            hovered_rack_drop_idx = Some(r_idx);
                        }
                        rack_jack_positions.push((rack_id, res.jack_pos, res.strip_rect));
                    }

                    // Empty space right of the last rack strip -> left-click deselects, right-click opens menu.
                    let rem_w = ui.available_width().max(120.0);
                    let (empty_rect, empty_resp) =
                        ui.allocate_exact_size(Vec2::new(rem_w, strip_h), Sense::click());
                    let _ = empty_rect;
                    if empty_resp.clicked() {
                        muts.deselect_all = true;
                    }
                    empty_resp.context_menu(|ui| {
                        draw_empty_mixer_context_menu(
                            selected_id,
                            !app.expanded_param_slots.is_empty()
                                || !app.expanded_sidechain_slots.is_empty(),
                            &mut muts,
                            ui,
                        );
                    });
                });
            });
    });

    // Draw rack reorder drop indicator while dragging a rack strip
    if let (Some(drag_id), Some(drop_idx)) = (app.rack_drag_source, hovered_rack_drop_idx) {
        if let Some((_, _, target_rect)) = rack_jack_positions.get(drop_idx) {
            let drag_pos = app.session.racks.iter().position(|r| r.id == drag_id);
            let line_x = if drag_pos.is_some_and(|p| p < drop_idx) {
                target_rect.max.x + 1.0
            } else {
                target_rect.min.x - 1.0
            };
            ui.painter().line_segment(
                [egui::pos2(line_x, target_rect.min.y), egui::pos2(line_x, target_rect.max.y)],
                Stroke::new(3.0, FL_MINT_GREEN),
            );
        }
        if ui.input(|i| i.pointer.any_released()) {
            muts.reorder_rack = Some((drag_id, drop_idx));
            app.rack_drag_source = None;
        }
    } else if app.rack_drag_source.is_some() && ui.input(|i| i.pointer.any_released()) {
        app.rack_drag_source = None;
    }

    // Reserve the routing cable gutter directly beneath the strips (also right-clickable!)
    let (gutter_rect, gutter_resp) = ui.allocate_exact_size(
        Vec2::new(ui.available_width().max(200.0), CABLE_GUTTER_HEIGHT),
        Sense::click(),
    );
    ui.painter().rect_filled(gutter_rect, 0.0, FL_STRIP_WELL);
    ui.painter().line_segment(
        [gutter_rect.min, egui::pos2(gutter_rect.max.x, gutter_rect.min.y)],
        Stroke::new(1.0, BORDER_SUBTLE),
    );
    if gutter_resp.clicked() {
        muts.deselect_all = true;
    }
    gutter_resp.context_menu(|ui| {
        draw_empty_mixer_context_menu(
            selected_id,
            !app.expanded_param_slots.is_empty() || !app.expanded_sidechain_slots.is_empty(),
            &mut muts,
            ui,
        );
    });

    // Paint green Bezier cables (audio) and amber-mint sidechain cables
    draw_active_routing_cables(
        app,
        selected_id,
        selected_to_master,
        &selected_sends,
        &selected_sc_targets,
        master_jack_pos,
        &rack_jack_positions,
        cable_clip_rect,
        ui,
    );

    // Complete drag-to-connect if pointer was released while dragging a green cable.
    if let Some(drag_src_id) = app.cable_drag_source {
        if ui.input(|i| i.pointer.any_released()) {
            if let Some(target) = hovered_target {
                match target {
                    None => {
                        if let Some(src) = app.session.racks.iter_mut().find(|r| r.id == drag_src_id) {
                            src.route_to_master = true;
                            muts.param_dirty = true;
                        }
                    }
                    Some(dst_id) if dst_id != drag_src_id => {
                        muts.add_send = Some((drag_src_id, dst_id));
                    }
                    _ => {}
                }
            }
            app.cable_drag_source = None;
        }
    }

    // Inline Rename & Color popup if requested via right-click
    draw_rename_rack_popup(app, rename_anchor, &mut muts, ui.ctx());

    apply_strip_mutations(app, muts, ui.ctx());
}

fn collect_outgoing_sidechains(
    racks: &[RackConfig],
    src_rack_id: Uuid,
) -> Vec<(Uuid, Uuid, String, f32, bool)> {
    let mut out = Vec::new();
    for r in racks {
        if r.id == src_rack_id {
            continue;
        }
        for slot in &r.slots {
            if let Some(ref sc) = slot.sidechain {
                if sc.enabled {
                    if let SidechainSourceKind::Rack { rack_id, pre_fader } = sc.source {
                        if rack_id == src_rack_id {
                            out.push((r.id, slot.id, slot.name.clone(), sc.gain_db, pre_fader));
                            break;
                        }
                    }
                }
            }
        }
    }
    out
}

fn draw_mixer_toolbar(app: &mut StageHostApp, ui: &mut egui::Ui) {
    ui.horizontal(|ui| {
        ui.label(
            RichText::new(format!("MIXER ({})", app.session.racks.len()))
                .strong()
                .size(13.5)
                .color(TEXT_PRIMARY),
        );
        ui.separator();
        ui.add(
            egui::TextEdit::singleline(&mut app.new_rack_name)
                .hint_text("New rack name...")
                .desired_width(145.0),
        );
        if ui.button("+ Add Rack").clicked() {
            app.push_undo_checkpoint();
            let name = if app.new_rack_name.trim().is_empty() {
                format!("Insert {}", app.session.racks.len() + 1)
            } else {
                app.new_rack_name.trim().to_string()
            };
            let rack = RackConfig::new(name);
            let new_id = rack.id;
            app.add_rack_incremental(rack);
            app.selected_rack_id = Some(new_id);
            app.selected_rack_ids = vec![new_id];
            app.new_rack_name.clear();
            app.mark_autosave_dirty();
        }
        if (!app.expanded_param_slots.is_empty() || !app.expanded_sidechain_slots.is_empty())
            && ui.button("Collapse Drawers").clicked()
        {
            app.expanded_param_slots.clear();
            app.expanded_sidechain_slots.clear();
        }
        if app.selected_rack_ids.len() > 1 {
            ui.label(
                RichText::new(format!("{} Selected", app.selected_rack_ids.len()))
                    .small()
                    .strong()
                    .color(ACCENT_AMBER),
            );
        }

        let rem_w = ui.available_width();
        if rem_w > 110.0 {
            ui.allocate_ui_with_layout(
                Vec2::new(rem_w, 20.0),
                egui::Layout::right_to_left(egui::Align::Center),
                |ui| {
                    ui.add(
                        egui::Label::new(
                            RichText::new(
                                "Ctrl+Click: Multi-select • Drag header: Reorder • Right-click: Menu",
                            )
                            .small()
                            .color(TEXT_MUTED),
                        )
                        .truncate(),
                    );
                },
            );
        }
    });
}

fn draw_empty_mixer_placeholder(app: &mut StageHostApp, ui: &mut egui::Ui) {
    let mut muts = StripMutations::default();
    let avail = ui.available_size();
    let (bg_rect, bg_resp) = ui.allocate_exact_size(avail.max(Vec2::new(200.0, 200.0)), Sense::click());
    ui.painter().rect_filled(bg_rect, 2.0, BG_WELL);
    ui.painter().rect_stroke(bg_rect, 2.0, Stroke::new(1.0, BORDER_HAIRLINE), StrokeKind::Inside);

    ui.scope_builder(egui::UiBuilder::new().max_rect(bg_rect.shrink(16.0)), |ui| {
        ui.label(RichText::new("No racks in this session.").strong().color(TEXT_PRIMARY));
        ui.label(
            RichText::new("Create a mixer strip rack or right-click empty area to add racks / load settings.")
                .small()
                .color(TEXT_SECONDARY),
        );
        ui.add_space(6.0);
        if ui.button("+ Create Main FX Strip").clicked() {
            muts.add_rack = Some(true);
        }
    });

    bg_resp.context_menu(|ui| {
        draw_empty_mixer_context_menu(None, false, &mut muts, ui);
    });
    apply_strip_mutations(app, muts, ui.ctx());
}

fn draw_empty_mixer_context_menu(
    selected_id: Option<Uuid>,
    has_expanded_drawers: bool,
    muts: &mut StripMutations,
    ui: &mut egui::Ui,
) {
    if ui.button("+ Add Rack (Live Input)").clicked() {
        muts.add_rack = Some(true);
        ui.close();
    }
    if ui.button("+ Add Aux Bus Rack (No Live Input)").clicked() {
        muts.add_rack = Some(false);
        ui.close();
    }
    if let Some(sid) = selected_id {
        if ui.button("Duplicate Selected Rack").clicked() {
            muts.duplicate_rack = Some(sid);
            ui.close();
        }
    }
    if has_expanded_drawers && ui.button("Collapse All Plugin Drawers").clicked() {
        // Handled inline via flag? Keep simple via toolbar or add toggle if needed
        ui.close();
    }
    ui.separator();
    if ui.button("Save All Settings to File...").clicked() {
        muts.save_all_settings = true;
        ui.close();
    }
    if ui.button("Load All Settings from File...").clicked() {
        muts.load_all_settings = true;
        ui.close();
    }
    ui.separator();
    if ui.button("Audio & Plugin Options...").clicked() {
        muts.open_settings = true;
        ui.close();
    }
}

fn draw_input_mixer_strip(
    app: &mut StageHostApp,
    strip_h: f32,
    meter_zone_h: f32,
    fader_h: f32,
    muts: &mut StripMutations,
    ui: &mut egui::Ui,
) {
    let frame_resp = egui::Frame::new()
        .fill(FL_STRIP_BG_ALT)
        .stroke(Stroke::new(1.0, BORDER_SUBTLE))
        .inner_margin(egui::Margin::symmetric(3, 3))
        .show(ui, |ui| {
            let inner_w = STRIP_WIDTH - 6.0;
            let inner_h = strip_h - 6.0;
            ui.set_min_size(Vec2::new(inner_w, inner_h));
            ui.set_max_size(Vec2::new(inner_w, inner_h));
            ui.vertical_centered(|ui| {
                ui.spacing_mut().item_spacing.y = 3.0;

                // 1. Header "IN"
                let hdr_rect = ui
                    .allocate_exact_size(Vec2::new(STRIP_WIDTH - 8.0, ROW_HDR_H), Sense::hover())
                    .0;
                ui.painter().rect_filled(hdr_rect, 1.0, Color32::from_rgb(30, 56, 62));
                ui.painter().text(
                    hdr_rect.center(),
                    egui::Align2::CENTER_CENTER,
                    "IN",
                    egui::FontId::monospace(10.0),
                    FL_MINT_GREEN,
                );

                // 2. Rotated Name + Vertical Meter
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 3.0;
                    let name_rect = ui
                        .allocate_exact_size(Vec2::new(22.0, meter_zone_h), Sense::hover())
                        .0;
                    theme::draw_rotated_vertical_text(ui, name_rect, "Live In", TEXT_SECONDARY, 11.0);
                    theme::draw_vertical_stereo_meter(
                        ui,
                        app.telemetry.input_peak_l,
                        app.telemetry.input_peak_r,
                        22.0,
                        meter_zone_h,
                    );
                });

                // 3. Green LED enable toggle
                let led_resp = draw_mint_led_button(
                    ui,
                    app.input_enabled,
                    if app.input_enabled {
                        "Input ON (click to mute)"
                    } else {
                        "Input MUTED (click to enable)"
                    },
                );
                if led_resp.clicked() {
                    app.input_enabled = !app.input_enabled;
                    app.sync_input_config_to_engine();
                    app.save_preferences();
                }

                // 4. Input mode button (identical height ROW_CTRL_H)
                ui.allocate_ui(Vec2::new(48.0, ROW_CTRL_H), |ui| {
                    ui.spacing_mut().button_padding = Vec2::new(2.0, 1.0);
                    if ui
                        .add_sized(
                            [48.0, ROW_CTRL_H],
                            egui::Button::new(
                                RichText::new(input_mode_label(app.input_mode)).size(10.0),
                            ),
                        )
                        .on_hover_text("Cycle input channel mode: ST, L->ST, R->ST, SUM")
                        .clicked()
                    {
                        app.input_mode = next_input_mode(app.input_mode);
                        app.sync_input_config_to_engine();
                        app.save_preferences();
                    }
                });

                // 5. Vertical Input Gain Fader
                let in_fader = theme::draw_vertical_fader(
                    ui,
                    &mut app.input_gain_db,
                    -36.0..=24.0,
                    0.0,
                    40.0,
                    fader_h,
                    false,
                    None,
                )
                .on_hover_ui(|ui| {
                    ui.label(format!(
                        "Input Gain: {:+.1} dB (double-click to reset)",
                        app.input_gain_db
                    ));
                });
                if in_fader.changed() {
                    app.sync_input_config_to_engine();
                }
                if in_fader.drag_stopped() || in_fader.double_clicked() {
                    app.save_preferences();
                }

                // 6. dB Readout
                ui.allocate_ui(Vec2::new(46.0, ROW_DB_H), |ui| {
                    ui.centered_and_justified(|ui| {
                        ui.label(
                            RichText::new(format!("{:+.1}", app.input_gain_db))
                                .size(9.5)
                                .monospace()
                                .color(TEXT_MUTED),
                        );
                    });
                });

                // 7. Status row spacer
                ui.allocate_exact_size(Vec2::new(46.0, ROW_STATUS_H), Sense::hover());

                // 8. Bottom socket spacer (identical ROW_SOCKET_H)
                let (bot_rect, _) =
                    ui.allocate_exact_size(Vec2::new(36.0, ROW_SOCKET_H), Sense::hover());
                ui.painter().text(
                    bot_rect.center(),
                    egui::Align2::CENTER_CENTER,
                    if app.input_enabled { "MIC" } else { "OFF" },
                    egui::FontId::monospace(8.5),
                    TEXT_MUTED,
                );
            });
        });

    frame_resp.response.context_menu(|ui| {
        if ui
            .button(if app.input_enabled {
                "Mute Live Input"
            } else {
                "Enable Live Input"
            })
            .clicked()
        {
            app.input_enabled = !app.input_enabled;
            app.sync_input_config_to_engine();
            app.save_preferences();
            ui.close();
        }
        ui.menu_button("Input Channel Mode", |ui| {
            for (mode, label) in [
                (InputChannelMode::Stereo, "Stereo (1+2)"),
                (InputChannelMode::MonoLeft, "Mono Left -> Stereo"),
                (InputChannelMode::MonoRight, "Mono Right -> Stereo"),
                (InputChannelMode::MonoSum, "Mono Sum (L+R)"),
            ] {
                if ui.selectable_label(app.input_mode == mode, label).clicked() {
                    app.input_mode = mode;
                    app.sync_input_config_to_engine();
                    app.save_preferences();
                    ui.close();
                }
            }
        });
        if ui.button("Reset Input Gain (0.0 dB)").clicked() {
            app.input_gain_db = 0.0;
            app.sync_input_config_to_engine();
            app.save_preferences();
            ui.close();
        }
        ui.separator();
        if ui.button("Audio & Plugin Options...").clicked() {
            muts.open_settings = true;
            ui.close();
        }
    });
}

#[allow(clippy::too_many_arguments)]
fn draw_master_mixer_strip(
    app: &mut StageHostApp,
    selected_rack_id: Option<Uuid>,
    selected_to_master: bool,
    strip_h: f32,
    meter_zone_h: f32,
    fader_h: f32,
    muts: &mut StripMutations,
    ui: &mut egui::Ui,
) -> (Pos2, bool) {
    let mut jack_pos = Pos2::ZERO;
    let mut jack_hovered = false;

    let frame_resp = egui::Frame::new()
        .fill(FL_STRIP_BG)
        .stroke(Stroke::new(1.0, BORDER_HAIRLINE))
        .inner_margin(egui::Margin::symmetric(3, 3))
        .show(ui, |ui| {
            let inner_w = STRIP_WIDTH - 6.0;
            let inner_h = strip_h - 6.0;
            ui.set_min_size(Vec2::new(inner_w, inner_h));
            ui.set_max_size(Vec2::new(inner_w, inner_h));
            ui.vertical_centered(|ui| {
                ui.spacing_mut().item_spacing.y = 3.0;

                // 1. Header "M"
                let hdr_rect = ui
                    .allocate_exact_size(Vec2::new(STRIP_WIDTH - 8.0, ROW_HDR_H), Sense::hover())
                    .0;
                ui.painter().rect_filled(hdr_rect, 1.0, Color32::from_rgb(52, 56, 64));
                ui.painter().text(
                    hdr_rect.center(),
                    egui::Align2::CENTER_CENTER,
                    "M",
                    egui::FontId::monospace(10.5),
                    TEXT_PRIMARY,
                );

                // 2. Rotated "Master" + Vertical Peak Meter
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 3.0;
                    let name_rect = ui
                        .allocate_exact_size(Vec2::new(22.0, meter_zone_h), Sense::hover())
                        .0;
                    theme::draw_rotated_vertical_text(ui, name_rect, "Master", TEXT_PRIMARY, 11.5);
                    theme::draw_vertical_stereo_meter(
                        ui,
                        app.telemetry.master_peak_l,
                        app.telemetry.master_peak_r,
                        22.0,
                        meter_zone_h,
                    );
                });

                // 3. Monitor ON/MUTE green LED
                let mon_on = !app.output_muted;
                let led_resp = draw_mint_led_button(
                    ui,
                    mon_on,
                    if mon_on {
                        "Master Monitor ON (click to mute)"
                    } else {
                        "Master Monitor MUTED (click to unmute)"
                    },
                );
                if led_resp.clicked() {
                    app.set_output_muted(!app.output_muted);
                }

                // 4. 0 dB reset pill (identical height ROW_CTRL_H)
                ui.allocate_ui(Vec2::new(48.0, ROW_CTRL_H), |ui| {
                    ui.spacing_mut().button_padding = Vec2::new(2.0, 1.0);
                    if ui
                        .add_sized(
                            [48.0, ROW_CTRL_H],
                            egui::Button::new(RichText::new("0 dB").size(10.0)),
                        )
                        .on_hover_text("Reset Master Fader to 0.0 dB")
                        .clicked()
                    {
                        app.session.master_gain_db = 0.0;
                        app.sync_master_gain_to_engine();
                    }
                });

                // 5. Master Gain Fader
                let master_db_display = app.session.master_gain_db;
                if theme::draw_vertical_fader(
                    ui,
                    &mut app.session.master_gain_db,
                    -48.0..=12.0,
                    0.0,
                    40.0,
                    fader_h,
                    false,
                    None,
                )
                .on_hover_ui(|ui| {
                    ui.label(format!(
                        "Master Gain: {:+.1} dB (double-click to reset)",
                        master_db_display
                    ));
                })
                .changed()
                {
                    app.sync_master_gain_to_engine();
                }

                // 6. dB Readout
                ui.allocate_ui(Vec2::new(46.0, ROW_DB_H), |ui| {
                    ui.centered_and_justified(|ui| {
                        ui.label(
                            RichText::new(format!("{:+.1}", app.session.master_gain_db))
                                .size(9.5)
                                .monospace()
                                .color(TEXT_MUTED),
                        );
                    });
                });

                // 7. Status row spacer
                ui.allocate_exact_size(Vec2::new(46.0, ROW_STATUS_H), Sense::hover());

                // 8. Master Bottom Routing Socket (receives green cable from selected rack!)
                let (sock_rect, sock_resp) =
                    ui.allocate_exact_size(Vec2::new(36.0, ROW_SOCKET_H), Sense::click());
                jack_pos = sock_rect.center();
                jack_hovered = sock_resp.hovered();
                let is_dragging_cable = app.cable_drag_source.is_some();

                paint_target_routing_socket(
                    ui.painter(),
                    sock_rect,
                    selected_rack_id.is_some() && selected_to_master,
                    sock_resp.hovered() && (is_dragging_cable || selected_rack_id.is_some()),
                    true,
                );
                if sock_resp
                    .on_hover_text(if selected_to_master {
                        "Connected to Master (click to unroute selected rack from Master)"
                    } else {
                        "Click or drop green cable here to route selected rack to Master"
                    })
                    .clicked()
                {
                    if let Some(sid) = selected_rack_id {
                        muts.toggle_master_route = Some(sid);
                    }
                }
            });
        });

    frame_resp.response.context_menu(|ui| {
        if ui
            .button(if app.output_muted {
                "Unmute Master Output"
            } else {
                "Mute Master Output"
            })
            .clicked()
        {
            app.set_output_muted(!app.output_muted);
            ui.close();
        }
        if ui.button("Reset Master Gain (0.0 dB)").clicked() {
            app.session.master_gain_db = 0.0;
            app.sync_master_gain_to_engine();
            ui.close();
        }
        ui.separator();
        if ui.button("Audio & Plugin Options...").clicked() {
            muts.open_settings = true;
            ui.close();
        }
    });

    (jack_pos, jack_hovered)
}

struct RackStripResult {
    jack_pos: Pos2,
    strip_rect: Rect,
    hovered_jack: bool,
    strip_hovered: bool,
}

#[allow(clippy::too_many_arguments)]
fn draw_single_rack_strip(
    rack: &mut RackConfig,
    rack_index: usize,
    rack_total: usize,
    is_selected: bool,
    is_multi_selected: bool,
    selected_rack_ids: &[Uuid],
    all_rack_names: &[(Uuid, String)],
    selected_rack_id: Option<Uuid>,
    send_from_selected: Option<&RackSendConfig>,
    sc_from_selected: Option<&(Uuid, String, f32, bool)>,
    target_sc_slots: &[(Uuid, String, Option<SidechainConfig>)],
    edited_send_level: &mut Option<(f32, bool)>,
    edited_sc_gain: &mut Option<(Uuid, f32)>,
    meters: (f32, f32),
    rack_latency_samples: u32,
    sample_rate: u32,
    audio_running: bool,
    strip_h: f32,
    meter_zone_h: f32,
    fader_h: f32,
    cable_drag_source: &mut Option<Uuid>,
    rack_drag_source: &mut Option<Uuid>,
    muts: &mut StripMutations,
    ui: &mut egui::Ui,
) -> RackStripResult {
    let base_bg = if rack_index.is_multiple_of(2) {
        FL_STRIP_BG
    } else {
        FL_STRIP_BG_ALT
    };
    let highlighted = is_selected || is_multi_selected;
    let strip_bg = theme::blend_strip_tint(base_bg, rack.color, if highlighted { 0.36 } else { 0.25 });
    let header_bg = theme::blend_strip_tint(
        Color32::from_rgb(48, 52, 58),
        rack.color,
        if highlighted { 0.60 } else { 0.42 },
    );

    let mut jack_pos = Pos2::ZERO;
    let mut hovered_jack = false;
    let rack_id = rack.id;
    let mut child_rclick_responses: Vec<egui::Response> = Vec::new();
    let ctrl_held = ui.input(|i| i.modifiers.ctrl || i.modifiers.command);

    // Always use 1.0px frame stroke so outer dimensions match IN and M identically;
    // selected highlight is drawn inside via StrokeKind::Inside.
    let frame_resp = egui::Frame::new()
        .fill(strip_bg)
        .stroke(Stroke::new(1.0, BORDER_SUBTLE))
        .inner_margin(egui::Margin::symmetric(3, 3))
        .show(ui, |ui| {
            let inner_w = STRIP_WIDTH - 6.0;
            let inner_h = strip_h - 6.0;
            ui.set_min_size(Vec2::new(inner_w, inner_h));
            ui.set_max_size(Vec2::new(inner_w, inner_h));
            ui.vertical_centered(|ui| {
                ui.spacing_mut().item_spacing.y = 3.0;

                // 1. Track Index Header (Ctrl+Click to multi-select, drag left/right to reorder)
                let (hdr_rect, hdr_resp) = ui.allocate_exact_size(
                    Vec2::new(STRIP_WIDTH - 8.0, ROW_HDR_H),
                    Sense::click_and_drag(),
                );
                ui.painter().rect_filled(hdr_rect, 1.0, header_bg);
                if is_selected {
                    ui.painter().rect_stroke(
                        hdr_rect,
                        1.0,
                        Stroke::new(1.0, FL_SELECT_BORDER),
                        StrokeKind::Inside,
                    );
                } else if is_multi_selected {
                    ui.painter().rect_stroke(
                        hdr_rect,
                        1.0,
                        Stroke::new(1.0, ACCENT_AMBER),
                        StrokeKind::Inside,
                    );
                }
                ui.painter().text(
                    hdr_rect.center(),
                    egui::Align2::CENTER_CENTER,
                    format!("{}", rack_index + 1),
                    egui::FontId::monospace(10.0),
                    if highlighted { TEXT_PRIMARY } else { TEXT_SECONDARY },
                );
                if hdr_resp.drag_started() {
                    *rack_drag_source = Some(rack_id);
                }
                if hdr_resp.clicked() {
                    if ctrl_held {
                        muts.toggle_multi_select = Some(rack_id);
                    } else {
                        muts.select_rack = Some(rack_id);
                    }
                }
                let hdr_resp = hdr_resp.on_hover_text(
                    "Click: Select • Ctrl+Click: Multi-select • Drag: Reorder • Right-click: Menu",
                );
                child_rclick_responses.push(hdr_resp);

                // 2. Vertical Rack Name + Vertical Stereo Meter
                let (zone_rect, zone_resp) = ui.allocate_exact_size(
                    Vec2::new(STRIP_WIDTH - 8.0, meter_zone_h),
                    Sense::click_and_drag(),
                );
                if zone_resp.drag_started() {
                    *rack_drag_source = Some(rack_id);
                }
                if zone_resp.clicked() {
                    if ctrl_held {
                        muts.toggle_multi_select = Some(rack_id);
                    } else {
                        muts.select_rack = Some(rack_id);
                    }
                }
                if is_selected {
                    ui.painter().rect_stroke(
                        zone_rect,
                        1.0,
                        Stroke::new(1.0, FL_MINT_GREEN),
                        StrokeKind::Inside,
                    );
                } else if is_multi_selected {
                    ui.painter().rect_stroke(
                        zone_rect,
                        1.0,
                        Stroke::new(1.0, ACCENT_AMBER),
                        StrokeKind::Inside,
                    );
                }
                let name_rect = Rect::from_min_max(
                    zone_rect.min,
                    egui::pos2(zone_rect.min.x + 22.0, zone_rect.max.y),
                );
                let name_color = if is_selected {
                    FL_MINT_GREEN
                } else if rack.enabled && !rack.muted {
                    TEXT_PRIMARY
                } else {
                    TEXT_MUTED
                };
                theme::draw_rotated_vertical_text(ui, name_rect, &rack.name, name_color, 11.0);

                let meter_area = Rect::from_min_max(
                    egui::pos2(zone_rect.min.x + 25.0, zone_rect.min.y + 2.0),
                    egui::pos2(zone_rect.max.x - 2.0, zone_rect.max.y - 2.0),
                );
                paint_strip_meter(ui, rack_id, meter_area, meters.0, meters.1);
                child_rclick_responses.push(zone_resp);

                // 3. Green Enable/Power LED
                let led_resp = draw_mint_led_button(
                    ui,
                    rack.enabled,
                    if rack.enabled {
                        "Rack Enabled (click to disable)"
                    } else {
                        "Rack Disabled (click to enable)"
                    },
                );
                if led_resp.clicked() {
                    rack.enabled = !rack.enabled;
                    muts.param_dirty = true;
                    if !is_multi_selected && !ctrl_held {
                        muts.select_rack = Some(rack_id);
                    }
                }
                child_rclick_responses.push(led_resp);

                // 4. Micro toggles: In (receive_input) / M (muted) / S (solo)
                ui.allocate_ui(Vec2::new(48.0, ROW_CTRL_H), |ui| {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 1.5;
                        if draw_micro_toggle(
                            ui,
                            &mut rack.receive_input,
                            "I",
                            ACCENT_AMBER,
                            TEXT_ON_ACCENT,
                            "Receive Live Audio Input (off = Aux Bus only)",
                        ) {
                            muts.param_dirty = true;
                            if !is_multi_selected && !ctrl_held {
                                muts.select_rack = Some(rack_id);
                            }
                        }
                        if draw_micro_toggle(
                            ui,
                            &mut rack.muted,
                            "M",
                            AUDIO_RED_ACTIVE,
                            TEXT_PRIMARY,
                            "Mute Rack",
                        ) {
                            muts.param_dirty = true;
                            if !is_multi_selected && !ctrl_held {
                                muts.select_rack = Some(rack_id);
                            }
                        }
                        if draw_micro_toggle(
                            ui,
                            &mut rack.solo,
                            "S",
                            ACCENT_AMBER,
                            TEXT_ON_ACCENT,
                            "Solo Rack",
                        ) {
                            muts.param_dirty = true;
                            if !is_multi_selected && !ctrl_held {
                                muts.select_rack = Some(rack_id);
                            }
                        }
                    });
                });

                // 5. Vertical Volume Fader (identical fader_h to IN & M!)
                let prev_gain_db = rack.gain_db;
                let rack_name_ref = rack.name.as_str();
                let is_grouped = rack.group_id.is_some();
                let fader_resp = theme::draw_vertical_fader(
                    ui,
                    &mut rack.gain_db,
                    -48.0..=12.0,
                    0.0,
                    40.0,
                    fader_h,
                    is_selected,
                    None,
                )
                .on_hover_ui(|ui| {
                    let grp_hint = if is_grouped {
                        "\nGrouped fader: moves all racks in this group"
                    } else {
                        ""
                    };
                    ui.label(format!(
                        "{rack_name_ref} Gain: {:+.1} dB (double-click to reset){grp_hint}",
                        rack.gain_db
                    ));
                });
                if fader_resp.changed() {
                    muts.param_dirty = true;
                    let delta = rack.gain_db - prev_gain_db;
                    if delta.abs() >= 1e-4 {
                        muts.gain_delta = Some((rack_id, rack.group_id, delta));
                    }
                }
                if (fader_resp.clicked() || fader_resp.drag_started())
                    && !is_multi_selected
                    && !ctrl_held
                {
                    muts.select_rack = Some(rack_id);
                }
                child_rclick_responses.push(fader_resp);

                // 6. dB Readout
                ui.allocate_ui(Vec2::new(46.0, ROW_DB_H), |ui| {
                    ui.centered_and_justified(|ui| {
                        ui.label(
                            RichText::new(format!("{:+.1}", rack.gain_db))
                                .size(9.5)
                                .monospace()
                                .color(if is_selected { TEXT_PRIMARY } else { TEXT_MUTED }),
                        );
                    });
                });

                // 7. FX active, Group link & Latency clock indicator strip
                let active_slots = rack.slots.iter().filter(|s| s.enabled && !s.bypassed).count();
                let (status_rect, status_resp) =
                    ui.allocate_exact_size(Vec2::new(46.0, ROW_STATUS_H), Sense::click());
                let fx_center = egui::pos2(status_rect.center().x - 12.0, status_rect.center().y);
                let grp_center = status_rect.center();
                let lat_center = egui::pos2(status_rect.center().x + 12.0, status_rect.center().y);
                let fx_color = if active_slots > 0 {
                    AUDIO_YELLOW
                } else {
                    Color32::from_rgb(66, 70, 78)
                };
                ui.painter().circle_filled(fx_center, 3.5, fx_color);

                if rack.group_id.is_some() {
                    ui.painter().text(
                        grp_center,
                        egui::Align2::CENTER_CENTER,
                        "G",
                        egui::FontId::monospace(9.0),
                        FL_MINT_GREEN,
                    );
                }

                let delay_ms = format_delay_ms(rack_latency_samples, sample_rate);
                let lat_color = if audio_running && rack_latency_samples > 0 {
                    FL_MINT_GREEN
                } else {
                    Color32::from_rgb(66, 70, 78)
                };
                ui.painter().circle_stroke(lat_center, 3.8, Stroke::new(1.2, lat_color));
                ui.painter().line_segment(
                    [lat_center, lat_center + Vec2::new(0.0, -2.2)],
                    Stroke::new(1.0, lat_color),
                );
                let has_group = rack.group_id.is_some();
                let slot_total = rack.slots.len();
                if status_resp
                    .on_hover_ui(|ui| {
                        let grp_txt = if has_group {
                            " | Linked Group Fader"
                        } else {
                            ""
                        };
                        if audio_running && rack_latency_samples > 0 {
                            ui.label(format!(
                                "{active_slots}/{slot_total} FX active | Delay: {rack_latency_samples} smp ({delay_ms:.2} ms){grp_txt}"
                            ));
                        } else {
                            ui.label(format!(
                                "{active_slots}/{slot_total} FX active | Zero plugin delay{grp_txt}"
                            ));
                        }
                    })
                    .clicked()
                {
                    muts.select_rack = Some(rack_id);
                }

                // 8. Bottom Routing Socket (Audio Green Knob AND/OR Sidechain Volume Knob)
                let (sock_rect, sock_resp) =
                    ui.allocate_exact_size(Vec2::new(46.0, ROW_SOCKET_H), Sense::click_and_drag());
                jack_pos = sock_rect.center();
                hovered_jack = sock_resp.hovered();

                if is_selected {
                    paint_source_routing_plug(ui.painter(), sock_rect, sock_resp.hovered());
                    if sock_resp.drag_started() {
                        *cable_drag_source = Some(rack_id);
                    }
                    sock_resp.on_hover_text(
                        "Drag green cable to another rack or Master (M) to route audio",
                    );
                } else {
                    match (send_from_selected, sc_from_selected) {
                        // Both audio send AND sidechain send active: draw two side-by-side mini knobs!
                        (Some(send), Some((sc_slot_id, sc_slot_name, sc_db, _pre))) => {
                            let left_knob_rect = Rect::from_center_size(
                                egui::pos2(sock_rect.center().x - 11.0, sock_rect.center().y),
                                Vec2::splat(20.0),
                            );
                            let right_knob_rect = Rect::from_center_size(
                                egui::pos2(sock_rect.center().x + 11.0, sock_rect.center().y),
                                Vec2::splat(20.0),
                            );
                            let mut lvl = send.level_db;
                            let k1 = ui
                                .put(left_knob_rect, |ui: &mut egui::Ui| {
                                    theme::draw_rotary_knob(
                                        ui,
                                        ui.make_persistent_id(("send_knob", selected_rack_id, rack_id)),
                                        &mut lvl,
                                        -48.0..=6.0,
                                        -6.0,
                                        20.0,
                                        if send.enabled { FL_CABLE_GREEN } else { TEXT_MUTED },
                                        false,
                                    )
                                })
                                .on_hover_ui(|ui| {
                                    ui.label(format!(
                                        "Audio Send -> {}: {:+.1} dB{}\nRight-click: routing/sidechain options",
                                        rack.name,
                                        lvl,
                                        if send.enabled { "" } else { " (Muted)" }
                                    ));
                                });
                            if k1.changed() {
                                *edited_send_level = Some((lvl, true));
                            }
                            let mut sc_lvl = *sc_db;
                            let k2 = ui
                                .put(right_knob_rect, |ui: &mut egui::Ui| {
                                    theme::draw_rotary_knob(
                                        ui,
                                        ui.make_persistent_id(("sc_send_knob", selected_rack_id, rack_id)),
                                        &mut sc_lvl,
                                        -48.0..=24.0,
                                        0.0,
                                        20.0,
                                        ACCENT_AMBER,
                                        false,
                                    )
                                })
                                .on_hover_ui(|ui| {
                                    ui.label(format!(
                                        "Sidechain Send -> {} ({}): {:+.1} dB\nDouble-click: 0.0 dB | Right-click: SC options",
                                        rack.name, sc_slot_name, sc_lvl
                                    ));
                                });
                            if k2.changed() {
                                *edited_sc_gain = Some((*sc_slot_id, sc_lvl));
                            }
                            k1.context_menu(|ui| {
                                draw_socket_routing_context_menu(
                                    selected_rack_id,
                                    rack_id,
                                    &rack.name,
                                    Some(send),
                                    sc_from_selected,
                                    target_sc_slots,
                                    muts,
                                    ui,
                                );
                            });
                            k2.context_menu(|ui| {
                                draw_socket_routing_context_menu(
                                    selected_rack_id,
                                    rack_id,
                                    &rack.name,
                                    Some(send),
                                    sc_from_selected,
                                    target_sc_slots,
                                    muts,
                                    ui,
                                );
                            });
                        }
                        // Audio send only
                        (Some(send), None) => {
                            let mut lvl = send.level_db;
                            let knob_r = ui
                                .put(sock_rect, |ui: &mut egui::Ui| {
                                    theme::draw_rotary_knob(
                                        ui,
                                        ui.make_persistent_id(("send_knob", selected_rack_id, rack_id)),
                                        &mut lvl,
                                        -48.0..=6.0,
                                        -6.0,
                                        22.0,
                                        if send.enabled { FL_CABLE_GREEN } else { TEXT_MUTED },
                                        false,
                                    )
                                })
                                .on_hover_ui(|ui| {
                                    ui.label(format!(
                                        "Send -> {}: {:+.1} dB{}\nDouble-click: 0 dB | Right-click: audio/sidechain options",
                                        rack.name,
                                        lvl,
                                        if send.enabled { "" } else { " (Muted)" }
                                    ));
                                });
                            if knob_r.changed() {
                                *edited_send_level = Some((lvl, true));
                            }
                            knob_r.context_menu(|ui| {
                                draw_socket_routing_context_menu(
                                    selected_rack_id,
                                    rack_id,
                                    &rack.name,
                                    Some(send),
                                    None,
                                    target_sc_slots,
                                    muts,
                                    ui,
                                );
                            });
                        }
                        // Sidechain key send only
                        (None, Some((sc_slot_id, sc_slot_name, sc_db, _pre))) => {
                            let mut sc_lvl = *sc_db;
                            let knob_r = ui
                                .put(sock_rect, |ui: &mut egui::Ui| {
                                    theme::draw_rotary_knob(
                                        ui,
                                        ui.make_persistent_id(("sc_send_knob", selected_rack_id, rack_id)),
                                        &mut sc_lvl,
                                        -48.0..=24.0,
                                        0.0,
                                        22.0,
                                        ACCENT_AMBER,
                                        false,
                                    )
                                })
                                .on_hover_ui(|ui| {
                                    ui.label(format!(
                                        "Sidechain Send -> {} ({}): {:+.1} dB\nDouble-click: 0.0 dB | Right-click: options",
                                        rack.name, sc_slot_name, sc_lvl
                                    ));
                                });
                            if knob_r.changed() {
                                *edited_sc_gain = Some((*sc_slot_id, sc_lvl));
                            }
                            knob_r.context_menu(|ui| {
                                draw_socket_routing_context_menu(
                                    selected_rack_id,
                                    rack_id,
                                    &rack.name,
                                    None,
                                    sc_from_selected,
                                    target_sc_slots,
                                    muts,
                                    ui,
                                );
                            });
                        }
                        // Unconnected socket
                        (None, None) => {
                            let drop_hover = cable_drag_source.is_some() && sock_resp.hovered();
                            paint_target_routing_socket(
                                ui.painter(),
                                sock_rect,
                                false,
                                sock_resp.hovered() || drop_hover,
                                false,
                            );
                            let sock_resp = sock_resp.on_hover_ui(|ui| {
                                ui.label(format!(
                                    "Click: Route Audio -> {} | Right-click: Sidechain / Route menu",
                                    rack.name
                                ));
                            });
                            if sock_resp.clicked() {
                                if let Some(sid) = selected_rack_id {
                                    muts.add_send = Some((sid, rack_id));
                                }
                            }
                            sock_resp.context_menu(|ui| {
                                draw_socket_routing_context_menu(
                                    selected_rack_id,
                                    rack_id,
                                    &rack.name,
                                    None,
                                    None,
                                    target_sc_slots,
                                    muts,
                                    ui,
                                );
                            });
                        }
                    }
                }
            });
        });

    let strip_rect = frame_resp.response.rect;
    if is_selected {
        ui.painter().rect_stroke(
            strip_rect,
            0.0,
            Stroke::new(1.5, FL_SELECT_BORDER),
            StrokeKind::Inside,
        );
    } else if is_multi_selected {
        ui.painter().rect_stroke(
            strip_rect,
            0.0,
            Stroke::new(1.5, ACCENT_AMBER),
            StrokeKind::Inside,
        );
    }

    let strip_hovered = frame_resp.response.hovered()
        || ui
            .ctx()
            .pointer_latest_pos()
            .is_some_and(|p| strip_rect.contains(p));

    // Attach unified rack context menu across strip frame & interactive children
    let rack_snapshot = (rack.id, rack.name.clone(), rack.color, rack.receive_input, rack.route_to_master);
    frame_resp.response.context_menu(|ui| {
        draw_rack_context_menu(
            rack,
            rack_index,
            rack_total,
            selected_rack_ids,
            all_rack_names,
            muts,
            ui,
        );
    });
    for resp in child_rclick_responses {
        resp.context_menu(|ui| {
            draw_rack_context_menu_from_snapshot(
                rack_snapshot.0,
                &rack_snapshot.1,
                rack_snapshot.2,
                rack_snapshot.3,
                rack_snapshot.4,
                rack_index,
                rack_total,
                selected_rack_ids,
                all_rack_names,
                rack,
                muts,
                ui,
            );
        });
    }

    RackStripResult {
        jack_pos,
        strip_rect,
        hovered_jack,
        strip_hovered,
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_socket_routing_context_menu(
    selected_rack_id: Option<Uuid>,
    target_rack_id: Uuid,
    target_name: &str,
    existing_send: Option<&RackSendConfig>,
    existing_sc: Option<&(Uuid, String, f32, bool)>,
    target_sc_slots: &[(Uuid, String, Option<SidechainConfig>)],
    muts: &mut StripMutations,
    ui: &mut egui::Ui,
) {
    let Some(sid) = selected_rack_id else {
        return;
    };
    ui.label(RichText::new(format!("Route -> {target_name}")).small().strong());
    ui.separator();
    if let Some(send) = existing_send {
        if ui
            .button(if send.enabled {
                "Mute Audio Send"
            } else {
                "Unmute Audio Send"
            })
            .clicked()
        {
            muts.toggle_send = Some((sid, target_rack_id));
            ui.close();
        }
        if ui.button("Disconnect Audio Cable").clicked() {
            muts.remove_send = Some((sid, target_rack_id));
            ui.close();
        }
    } else if ui.button("Route Audio to This Rack (Green Cable)").clicked() {
        muts.add_send = Some((sid, target_rack_id));
        ui.close();
    }

    ui.separator();
    if let Some((_slot_id, slot_name, _db, pre_fader)) = existing_sc {
        ui.label(
            RichText::new(format!("Sidechain Active: {slot_name}"))
                .small()
                .color(ACCENT_AMBER),
        );
        if ui
            .button(if *pre_fader {
                "Switch Sidechain to Post-Fader"
            } else {
                "Switch Sidechain to Pre-Fader"
            })
            .clicked()
        {
            muts.set_sidechain_link = Some((sid, target_rack_id, None, !*pre_fader));
            ui.close();
        }
        if ui.button("Disconnect Sidechain").clicked() {
            muts.remove_sidechain_link = Some((sid, target_rack_id));
            ui.close();
        }
    } else if target_sc_slots.is_empty() {
        ui.label(
            RichText::new("Sidechain: target rack has no SC-capable plugin")
                .small()
                .color(TEXT_MUTED),
        );
    } else if target_sc_slots.len() == 1 {
        let (slot_id, slot_name, _) = &target_sc_slots[0];
        if ui
            .button(format!("Sidechain to '{slot_name}' (Key)"))
            .clicked()
        {
            muts.set_sidechain_link = Some((sid, target_rack_id, Some(*slot_id), false));
            ui.close();
        }
    } else {
        ui.menu_button("Sidechain to Plugin...", |ui| {
            for (slot_id, slot_name, _) in target_sc_slots {
                if ui.button(slot_name).clicked() {
                    muts.set_sidechain_link = Some((sid, target_rack_id, Some(*slot_id), false));
                    ui.close();
                }
            }
        });
    }
}

fn draw_rack_context_menu(
    rack: &mut RackConfig,
    rack_index: usize,
    rack_total: usize,
    selected_rack_ids: &[Uuid],
    all_rack_names: &[(Uuid, String)],
    muts: &mut StripMutations,
    ui: &mut egui::Ui,
) {
    let rack_id = rack.id;

    // Multi-Selection Group & Shared Keybind actions when >= 2 racks are Ctrl+Selected
    if selected_rack_ids.len() > 1 && selected_rack_ids.contains(&rack_id) {
        let count = selected_rack_ids.len();
        ui.label(
            RichText::new(format!("SELECTED GROUP ({count} Racks)"))
                .small()
                .strong()
                .color(ACCENT_AMBER),
        );
        if ui.button(format!("Group {count} Racks -> New Bus Rack")).clicked() {
            muts.group_selected_to_new_bus = true;
            ui.close();
        }
        if ui.button(format!("Link Faders ({count} Racks Group)")).clicked() {
            muts.link_selected_faders = true;
            ui.close();
        }
        let non_selected_targets: Vec<(Uuid, String)> = all_rack_names
            .iter()
            .filter(|(id, _)| !selected_rack_ids.contains(id))
            .cloned()
            .collect();
        if !non_selected_targets.is_empty() {
            ui.menu_button(format!("Group {count} Racks -> Existing Rack..."), |ui| {
                for (tid, tname) in non_selected_targets {
                    if ui.button(format!("Route to '{tname}' (Only)")).clicked() {
                        muts.group_selected_to_rack = Some(tid);
                        ui.close();
                    }
                }
            });
        }
        if ui
            .button(format!("Assign 1 Keybind for {count} Selected Racks..."))
            .clicked()
        {
            muts.bind_multi_rack = Some(selected_rack_ids.to_vec());
            ui.close();
        }
        ui.menu_button("Color All Selected", |ui| {
            for &(label, rgb) in FL_STRIP_PRESET_COLORS {
                let swatch = rgb.map(|[r, g, b]| Color32::from_rgb(r, g, b)).unwrap_or(FL_STRIP_BG);
                ui.horizontal(|ui| {
                    let (sr, _) = ui.allocate_exact_size(Vec2::splat(12.0), Sense::hover());
                    ui.painter().rect_filled(sr, 2.0, swatch);
                    if ui.button(label).clicked() {
                        muts.batch_color_selected = Some(rgb);
                        ui.close();
                    }
                });
            }
        });
        ui.horizontal(|ui| {
            if ui.small_button("Mute All").clicked() {
                muts.batch_mute_selected = Some(true);
                ui.close();
            }
            if ui.small_button("Unmute All").clicked() {
                muts.batch_mute_selected = Some(false);
                ui.close();
            }
            if ui.small_button("Enable All").clicked() {
                muts.batch_enable_selected = Some(true);
                ui.close();
            }
            if ui.small_button("Disable All").clicked() {
                muts.batch_enable_selected = Some(false);
                ui.close();
            }
        });
        if ui.button("Clear Multi-Selection").clicked() {
            muts.clear_multi_select = true;
            ui.close();
        }
        ui.separator();
    }

    if ui.button("Select Strip").clicked() {
        muts.select_rack = Some(rack_id);
        ui.close();
    }
    if ui.button("Rename / Change Color...").clicked() {
        muts.select_rack = Some(rack_id);
        muts.rename_rack = Some(rack_id);
        ui.close();
    }
    ui.menu_button("Strip Color", |ui| {
        for &(label, rgb) in FL_STRIP_PRESET_COLORS {
            let swatch = rgb.map(|[r, g, b]| Color32::from_rgb(r, g, b)).unwrap_or(FL_STRIP_BG);
            ui.horizontal(|ui| {
                let (sr, _) = ui.allocate_exact_size(Vec2::splat(12.0), Sense::hover());
                ui.painter().rect_filled(sr, 2.0, swatch);
                if ui.selectable_label(rack.color == rgb, label).clicked() {
                    rack.color = rgb;
                    muts.param_dirty = true;
                    ui.close();
                }
            });
        }
        ui.separator();
        ui.horizontal(|ui| {
            ui.label(RichText::new("Custom:").small());
            let mut rgb = rack.color.unwrap_or([64, 128, 160]);
            if ui.color_edit_button_srgb(&mut rgb).changed() {
                rack.color = Some(rgb);
                muts.param_dirty = true;
            }
        });
    });
    ui.separator();
    if ui
        .checkbox(&mut rack.receive_input, "Receive Live Input (In)")
        .changed()
    {
        muts.param_dirty = true;
    }
    if ui
        .checkbox(&mut rack.route_to_master, "Route to Master (M)")
        .changed()
    {
        muts.param_dirty = true;
    }
    ui.separator();
    if ui
        .add_enabled(rack_index > 0, egui::Button::new("Move Left"))
        .clicked()
    {
        muts.move_rack = Some((rack_index, -1));
        ui.close();
    }
    if ui
        .add_enabled(rack_index + 1 < rack_total, egui::Button::new("Move Right"))
        .clicked()
    {
        muts.move_rack = Some((rack_index, 1));
        ui.close();
    }
    if ui.button("Duplicate Rack").clicked() {
        muts.duplicate_rack = Some(rack_id);
        ui.close();
    }
    if ui.button("Assign Keybind...").clicked() {
        muts.bind_rack = Some(rack_id);
        ui.close();
    }
    if let Some(gid) = rack.group_id {
        ui.separator();
        if ui.button("Ungroup This Rack").clicked() {
            muts.ungroup_rack = Some(rack_id);
            ui.close();
        }
        if ui.button("Dissolve Fader Group").clicked() {
            muts.dissolve_group = Some(gid);
            ui.close();
        }
    }
    ui.separator();
    if ui
        .button(RichText::new("Delete Rack").color(AUDIO_RED))
        .clicked()
    {
        muts.delete_rack = Some(rack_id);
        ui.close();
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_rack_context_menu_from_snapshot(
    _id: Uuid,
    _name: &str,
    _color: Option<[u8; 3]>,
    _receive_input: bool,
    _route_to_master: bool,
    rack_index: usize,
    rack_total: usize,
    selected_rack_ids: &[Uuid],
    all_rack_names: &[(Uuid, String)],
    rack: &mut RackConfig,
    muts: &mut StripMutations,
    ui: &mut egui::Ui,
) {
    draw_rack_context_menu(
        rack,
        rack_index,
        rack_total,
        selected_rack_ids,
        all_rack_names,
        muts,
        ui,
    );
}

fn draw_rename_rack_popup(
    app: &mut StageHostApp,
    anchor: Option<Pos2>,
    muts: &mut StripMutations,
    ctx: &egui::Context,
) {
    let Some(rename_id) = app.renaming_rack_id else {
        return;
    };
    let Some(rack) = app.session.racks.iter_mut().find(|r| r.id == rename_id) else {
        app.renaming_rack_id = None;
        return;
    };
    let pos = anchor.unwrap_or_else(|| egui::pos2(240.0, 120.0));
    let mut close_popup = false;

    egui::Area::new(egui::Id::unique(("rename_rack_area", rename_id)))
        .fixed_pos(pos)
        .order(egui::Order::Foreground)
        .show(ctx, |ui| {
            egui::Frame::new()
                .fill(BG_PANEL)
                .stroke(Stroke::new(1.5, FL_SELECT_BORDER))
                .corner_radius(CornerRadius::same(3))
                .inner_margin(8.0)
                .show(ui, |ui| {
                    ui.set_width(210.0);
                    ui.label(RichText::new("Rename & Color Strip").small().strong());
                    let te = ui.add(
                        egui::TextEdit::singleline(&mut rack.name).desired_width(196.0),
                    );
                    if te.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        close_popup = true;
                    }
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        for &(_lbl, rgb) in FL_STRIP_PRESET_COLORS {
                            let c = rgb
                                .map(|[r, g, b]| Color32::from_rgb(r, g, b))
                                .unwrap_or(FL_STRIP_BG);
                            let (sr, sresp) =
                                ui.allocate_exact_size(Vec2::splat(16.0), Sense::click());
                            ui.painter().rect_filled(sr, 2.0, c);
                            if rack.color == rgb {
                                ui.painter().rect_stroke(
                                    sr,
                                    2.0,
                                    Stroke::new(1.5, TEXT_PRIMARY),
                                    StrokeKind::Inside,
                                );
                            }
                            if sresp.clicked() {
                                rack.color = rgb;
                                muts.param_dirty = true;
                            }
                        }
                        let mut custom = rack.color.unwrap_or([64, 128, 160]);
                        if ui.color_edit_button_srgb(&mut custom).changed() {
                            rack.color = Some(custom);
                            muts.param_dirty = true;
                        }
                    });
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        if ui.small_button("Done").clicked() {
                            close_popup = true;
                        }
                    });
                });
        });

    if close_popup || ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
        app.renaming_rack_id = None;
        muts.param_dirty = true;
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_active_routing_cables(
    app: &StageHostApp,
    selected_id: Option<Uuid>,
    selected_to_master: bool,
    selected_sends: &[RackSendConfig],
    selected_sc_targets: &[(Uuid, Uuid, String, f32, bool)],
    master_jack_pos: Option<Pos2>,
    rack_jack_positions: &[(Uuid, Pos2, Rect)],
    cable_clip_rect: Rect,
    ui: &egui::Ui,
) {
    let Some(sid) = selected_id else {
        return;
    };
    let Some((_, src_pos, _)) = rack_jack_positions.iter().find(|(id, _, _)| *id == sid) else {
        return;
    };

    let painter = ui.painter().with_clip_rect(cable_clip_rect);

    // Green cable to Master (M)
    if selected_to_master {
        if let Some(m_pos) = master_jack_pos {
            theme::draw_routing_cable(&painter, *src_pos, m_pos, true, 24.0);
        }
    }

    // Green cables to Aux/Bus target racks
    for (idx, send) in selected_sends.iter().enumerate() {
        if let Some((_, dst_pos, _)) = rack_jack_positions
            .iter()
            .find(|(id, _, _)| *id == send.target_rack_id)
        {
            let drop = 18.0 + ((idx % 4) as f32) * 5.0;
            theme::draw_routing_cable(&painter, *src_pos, *dst_pos, send.enabled, drop);
        }
    }

    // Amber-mint Sidechain cables to target racks
    for (idx, (dst_id, _, _, _, _)) in selected_sc_targets.iter().enumerate() {
        if let Some((_, dst_pos, _)) = rack_jack_positions.iter().find(|(id, _, _)| id == dst_id) {
            let drop = 14.0 + ((idx % 3) as f32) * 5.0;
            let cp1 = egui::pos2(src_pos.x, src_pos.y + drop);
            let cp2 = egui::pos2(dst_pos.x, dst_pos.y + drop);
            painter.add(egui::epaint::CubicBezierShape::from_points_stroke(
                [*src_pos, cp1, cp2, *dst_pos],
                false,
                Color32::TRANSPARENT,
                Stroke::new(1.5, ACCENT_AMBER),
            ));
        }
    }

    // Live dragged cable preview
    if let Some(drag_src_id) = app.cable_drag_source {
        if let Some((_, drag_pos, _)) = rack_jack_positions.iter().find(|(id, _, _)| *id == drag_src_id) {
            if let Some(pointer_pos) = ui.ctx().pointer_latest_pos() {
                theme::draw_routing_cable(&painter, *drag_pos, pointer_pos, true, 20.0);
            }
        }
    }
}

fn apply_strip_mutations(app: &mut StageHostApp, muts: StripMutations, ctx: &egui::Context) {
    if muts.deselect_all {
        app.selected_rack_id = None;
        app.selected_rack_ids.clear();
        app.renaming_rack_id = None;
        if !cfg!(test) {
            app.save_preferences();
        }
    }
    if let Some(sel) = muts.select_rack {
        app.selected_rack_id = Some(sel);
        app.selected_rack_ids = vec![sel];
        if !cfg!(test) {
            app.save_preferences();
        }
    }
    if let Some(toggle_id) = muts.toggle_multi_select {
        if let Some(pos) = app.selected_rack_ids.iter().position(|id| *id == toggle_id) {
            if app.selected_rack_ids.len() > 1 {
                app.selected_rack_ids.remove(pos);
                if app.selected_rack_id == Some(toggle_id) {
                    app.selected_rack_id = app.selected_rack_ids.last().copied();
                }
            }
        } else {
            if let Some(cur) = app.selected_rack_id {
                if !app.selected_rack_ids.contains(&cur) {
                    app.selected_rack_ids.push(cur);
                }
            }
            app.selected_rack_ids.push(toggle_id);
            app.selected_rack_id = Some(toggle_id);
        }
        if !cfg!(test) {
            app.save_preferences();
        }
    }
    if muts.clear_multi_select {
        app.selected_rack_ids = app.selected_rack_id.into_iter().collect();
    }
    if let Some(ren) = muts.rename_rack {
        app.renaming_rack_id = Some(ren);
    }

    let mut order_dirty = muts.order_dirty;
    let mut param_dirty = muts.param_dirty;

    if muts.group_selected_to_new_bus && app.selected_rack_ids.len() > 1 {
        app.push_undo_checkpoint();
        let bus_num = app.session.racks.iter().filter(|r| !r.receive_input).count() + 1;
        let mut bus = RackConfig::new(format!("Group {bus_num}"));
        bus.receive_input = false;
        bus.route_to_master = true;
        bus.color = Some([120, 58, 58]);
        let bus_id = bus.id;
        for rack in &mut app.session.racks {
            if app.selected_rack_ids.contains(&rack.id) {
                rack.group_id = Some(bus_id);
                rack.route_to_master = false;
                if let Some(existing) = rack.sends.iter_mut().find(|s| s.target_rack_id == bus_id) {
                    existing.enabled = true;
                    existing.level_db = 0.0;
                } else {
                    rack.sends.push(RackSendConfig {
                        target_rack_id: bus_id,
                        level_db: 0.0,
                        enabled: true,
                    });
                }
            }
        }
        app.add_rack_incremental(bus);
        app.selected_rack_id = Some(bus_id);
        app.selected_rack_ids = vec![bus_id];
        app.set_status(format!("Grouped racks into 'Group {bus_num}' (faders linked)"), false);
        param_dirty = true;
    }

    if let Some(target_id) = muts.group_selected_to_rack {
        if app.selected_rack_ids.len() > 1 {
            app.push_undo_checkpoint();
            for rack in &mut app.session.racks {
                if app.selected_rack_ids.contains(&rack.id) && rack.id != target_id {
                    rack.group_id = Some(target_id);
                    rack.route_to_master = false;
                    if let Some(existing) =
                        rack.sends.iter_mut().find(|s| s.target_rack_id == target_id)
                    {
                        existing.enabled = true;
                        existing.level_db = 0.0;
                    } else {
                        rack.sends.push(RackSendConfig {
                            target_rack_id: target_id,
                            level_db: 0.0,
                            enabled: true,
                        });
                    }
                }
            }
            app.set_status("Grouped & linked faders for selected racks", false);
            param_dirty = true;
        }
    }

    if muts.link_selected_faders && app.selected_rack_ids.len() > 1 {
        app.push_undo_checkpoint();
        let gid = Uuid::new_v4();
        let mut count = 0usize;
        for rack in &mut app.session.racks {
            if app.selected_rack_ids.contains(&rack.id) {
                rack.group_id = Some(gid);
                count += 1;
            }
        }
        app.set_status(format!("Linked faders for {count} selected racks"), false);
        param_dirty = true;
    }

    if let Some(rid) = muts.ungroup_rack {
        app.push_undo_checkpoint();
        if let Some(rack) = app.session.racks.iter_mut().find(|r| r.id == rid) {
            rack.group_id = None;
        }
        param_dirty = true;
    }

    if let Some(gid) = muts.dissolve_group {
        app.push_undo_checkpoint();
        for rack in &mut app.session.racks {
            if rack.group_id == Some(gid) {
                rack.group_id = None;
            }
        }
        param_dirty = true;
    }

    if let Some((src_id, src_gid, delta_db)) = muts.gain_delta {
        let selected_snapshot = app.selected_rack_ids.clone();
        propagate_grouped_gain_delta(
            &mut app.session.racks,
            &selected_snapshot,
            src_id,
            src_gid,
            delta_db,
        );
        param_dirty = true;
    }

    if let Some(color) = muts.batch_color_selected {
        app.push_undo_checkpoint();
        for rack in &mut app.session.racks {
            if app.selected_rack_ids.contains(&rack.id) {
                rack.color = color;
            }
        }
        param_dirty = true;
    }
    if let Some(muted) = muts.batch_mute_selected {
        app.push_undo_checkpoint();
        for rack in &mut app.session.racks {
            if app.selected_rack_ids.contains(&rack.id) {
                rack.muted = muted;
            }
        }
        param_dirty = true;
    }
    if let Some(enabled) = muts.batch_enable_selected {
        app.push_undo_checkpoint();
        for rack in &mut app.session.racks {
            if app.selected_rack_ids.contains(&rack.id) {
                rack.enabled = enabled;
            }
        }
        param_dirty = true;
    }

    if let Some(receive_input) = muts.add_rack {
        app.push_undo_checkpoint();
        let next_idx = app.session.racks.len() + 1;
        let mut rack = RackConfig::new(if receive_input {
            format!("Insert {next_idx}")
        } else {
            format!("Bus {next_idx}")
        });
        rack.receive_input = receive_input;
        let new_id = rack.id;
        app.add_rack_incremental(rack);
        app.selected_rack_id = Some(new_id);
        app.mark_autosave_dirty();
    }

    if let Some((src_id, dst_id)) = muts.add_send {
        app.push_undo_checkpoint();
        if let Some(src) = app.session.racks.iter_mut().find(|r| r.id == src_id) {
            if let Some(existing) = src.sends.iter_mut().find(|s| s.target_rack_id == dst_id) {
                existing.enabled = true;
            } else {
                src.sends.push(RackSendConfig {
                    target_rack_id: dst_id,
                    level_db: -6.0,
                    enabled: true,
                });
            }
            param_dirty = true;
        }
    }
    if let Some((src_id, dst_id)) = muts.toggle_send {
        if let Some(src) = app.session.racks.iter_mut().find(|r| r.id == src_id) {
            if let Some(send) = src.sends.iter_mut().find(|s| s.target_rack_id == dst_id) {
                send.enabled = !send.enabled;
                param_dirty = true;
            }
        }
    }
    if let Some((src_id, dst_id)) = muts.remove_send {
        app.push_undo_checkpoint();
        if let Some(src) = app.session.racks.iter_mut().find(|r| r.id == src_id) {
            src.sends.retain(|s| s.target_rack_id != dst_id);
            param_dirty = true;
        }
    }
    if let Some(sid) = muts.toggle_master_route {
        app.push_undo_checkpoint();
        if let Some(rack) = app.session.racks.iter_mut().find(|r| r.id == sid) {
            rack.route_to_master = !rack.route_to_master;
            param_dirty = true;
        }
    }
    if let Some((src_id, dst_id, slot_opt, pre_fader)) = muts.set_sidechain_link {
        app.push_undo_checkpoint();
        if let Some(dst_rack) = app.session.racks.iter_mut().find(|r| r.id == dst_id) {
            let target_slot = match slot_opt {
                Some(slot_id) => dst_rack.slots.iter_mut().find(|s| s.id == slot_id),
                None => dst_rack.slots.iter_mut().find(|s| {
                    matches!(
                        s.sidechain,
                        Some(ref sc) if matches!(sc.source, SidechainSourceKind::Rack { rack_id, .. } if rack_id == src_id)
                    )
                }),
            };
            if let Some(slot) = target_slot {
                let prev_db = slot.sidechain.as_ref().map_or(0.0, |sc| sc.gain_db);
                slot.sidechain = Some(SidechainConfig {
                    enabled: true,
                    source: SidechainSourceKind::Rack {
                        rack_id: src_id,
                        pre_fader,
                    },
                    gain_db: prev_db,
                });
                param_dirty = true;
            }
        }
    }
    if let Some((src_id, dst_id)) = muts.remove_sidechain_link {
        app.push_undo_checkpoint();
        if let Some(dst_rack) = app.session.racks.iter_mut().find(|r| r.id == dst_id) {
            for slot in &mut dst_rack.slots {
                if matches!(
                    slot.sidechain,
                    Some(ref sc) if matches!(sc.source, SidechainSourceKind::Rack { rack_id, .. } if rack_id == src_id)
                ) {
                    slot.sidechain = None;
                    param_dirty = true;
                }
            }
        }
    }

    if let Some((drag_id, drop_idx)) = muts.reorder_rack {
        if let Some(src_idx) = app.session.racks.iter().position(|r| r.id == drag_id) {
            if src_idx != drop_idx && drop_idx < app.session.racks.len() {
                app.push_undo_checkpoint();
                let item = app.session.racks.remove(src_idx);
                app.session.racks.insert(drop_idx, item);
                order_dirty = true;
            }
        }
    }

    if let Some(id) = muts.delete_rack {
        app.push_undo_checkpoint();
        app.remove_rack_incremental(id);
        app.mark_autosave_dirty();
    } else if let Some(id) = muts.duplicate_rack {
        app.duplicate_rack(id);
    } else if let Some((idx, delta)) = muts.move_rack {
        app.move_rack(idx, delta);
    }
    if let Some(rack_id) = muts.bind_rack {
        open_keybindings_window(app, ctx, Some(KeybindTarget::Rack(rack_id)));
    }
    if let Some(rack_ids) = muts.bind_multi_rack {
        open_keybindings_window(app, ctx, Some(KeybindTarget::MultiRack(rack_ids)));
    }
    if muts.open_settings {
        app.refresh_devices();
        open_options_window(app, ctx, None);
    }
    if muts.save_all_settings {
        if let Some(path) = file_dialog::pick_save_file(
            "Save All StageHost Settings",
            "stagehost.stagehost-settings.json",
            "StageHost All Settings (*.stagehost-settings.json)",
            "*.stagehost-settings.json",
        ) {
            app.export_all_settings_file(&path);
        }
    }
    if muts.load_all_settings {
        if let Some(path) = file_dialog::pick_open_file(
            "Load All StageHost Settings",
            "StageHost All Settings (*.stagehost-settings.json)",
            "*.stagehost-settings.json;*.json",
        ) {
            app.import_all_settings_file(&path);
        }
    }

    if order_dirty {
        app.sync_rack_order_to_engine();
        app.mark_autosave_dirty();
    }
    if param_dirty {
        app.sync_params_to_engine();
        app.mark_autosave_dirty();
    }
}

/// Renders the Right FX Rack Inspector for the currently selected strip.
pub fn draw_right_fx_inspector(app: &mut StageHostApp, ui: &mut egui::Ui) {
    let Some(selected_id) = ensure_valid_selected_rack(app) else {
        ui.vertical_centered(|ui| {
            ui.add_space(24.0);
            ui.label(RichText::new("No Rack Selected").strong().color(TEXT_SECONDARY));
            ui.add_space(4.0);
            ui.label(
                RichText::new("Click any mixer strip to edit its plugins, cables, and stereo width.")
                    .small()
                    .color(TEXT_MUTED),
            );
        });
        return;
    };
    let Some(r_idx) = app.session.racks.iter().position(|r| r.id == selected_id) else {
        return;
    };

    let all_rack_names: Vec<(Uuid, String)> = app
        .session
        .racks
        .iter()
        .map(|r| (r.id, r.name.clone()))
        .collect();
    let rack_count = app.session.racks.len();
    let latency_ctx = LatencyDisplayCtx {
        audio_running: app.audio_engine.is_running(),
        sample_rate: if app.telemetry.sample_rate > 0 {
            app.telemetry.sample_rate
        } else {
            app.selected_sample_rate.max(8_000)
        },
        slot_latencies: &app.slot_latencies,
    };
    let sidechain_ctx = SidechainDisplayCtx {
        audio_running: app.audio_engine.is_running(),
        slot_caps: &app.slot_sidechain_caps,
        scanned_plugins: &app.scanned_plugins,
    };

    let mut param_dirty = false;
    let mut slot_order_dirty = false;
    let mut remove_rack = false;
    let mut remove_slot_idx_out: Option<usize> = None;
    let mut duplicate_rack = false;
    let mut move_rack_delta: Option<isize> = None;
    let mut dup_slot_idx: Option<usize> = None;
    let mut reorder_slot: Option<(usize, usize)> = None;
    let mut open_editor_req: Option<(Uuid, Uuid, String)> = None;
    let mut bind_req: Option<KeybindTarget> = None;
    let mut slot_param_updates: Vec<(Uuid, Uuid, u32, f32)> = Vec::new();
    let mut sc_gain_updates: Vec<(Uuid, Uuid, f32)> = Vec::new();
    let mut inspector_gain_delta: Option<(Uuid, Option<Uuid>, f32)> = None;
    let prev_add_slot_target = app.add_slot_target_rack_id;

    // Collect outgoing sidechains from this rack BEFORE borrowing racks[r_idx] mutably
    let outgoing_sidechains = collect_outgoing_sidechains(&app.session.racks, selected_id);

    let rack = &mut app.session.racks[r_idx];
    let rack_id = rack.id;

    // 1. Inspector Header (no Key button, no < > reorder buttons; right-click for all rack actions!)
    draw_inspector_header(
        rack,
        r_idx,
        rack_count,
        &mut param_dirty,
        &mut remove_rack,
        &mut duplicate_rack,
        &mut move_rack_delta,
        &mut bind_req,
        ui,
    );
    ui.add_space(6.0);

    // 2. Dynamic FX Plugin Slots Stack (ONLY added plugins + one boxed '+' button!)
    let extra_rows = rack.sends.len() + outgoing_sidechains.len();
    let footer_reserve = 148.0 + (extra_rows as f32) * 26.0;
    let slots_max_h = (ui.available_height() - footer_reserve).max(110.0);
    egui::ScrollArea::vertical()
        .id_salt(("fx_inspector_slots", rack_id))
        .max_height(slots_max_h)
        .auto_shrink([false, false])
        .show(ui, |ui| {
            let mut move_swap: Option<(usize, usize)> = None;
            let mut remove_slot_idx: Option<usize> = None;
            let mut hovered_slot_drop_idx: Option<usize> = None;
            let mut slot_rects: Vec<Rect> = Vec::with_capacity(rack.slots.len());
            let total_slots = rack.slots.len();

            for (s_idx, slot) in rack.slots.iter_mut().enumerate() {
                let has_editor = app
                    .slot_ui_handles
                    .get(&slot.id)
                    .is_some_and(|h| h.has_editor());
                let sc_support = sidechain_ctx.slot_support(slot);
                let is_external = !matches!(slot.kind, PluginKind::BuiltInEffect(_));

                let slot_act = draw_inspector_slot_row(
                    slot,
                    s_idx,
                    total_slots,
                    rack_id,
                    has_editor,
                    is_external,
                    sc_support,
                    &latency_ctx,
                    &mut app.expanded_param_slots,
                    &mut app.expanded_sidechain_slots,
                    &mut app.slot_drag_source,
                    &mut open_editor_req,
                    &mut bind_req,
                    ui,
                );
                param_dirty |= slot_act.param_dirty;
                if slot_act.move_up && s_idx > 0 {
                    move_swap = Some((s_idx, s_idx - 1));
                }
                if slot_act.move_down && s_idx + 1 < total_slots {
                    move_swap = Some((s_idx, s_idx + 1));
                }
                if slot_act.duplicate {
                    dup_slot_idx = Some(s_idx);
                }
                if slot_act.remove {
                    remove_slot_idx = Some(s_idx);
                }
                if app
                    .slot_drag_source
                    .is_some_and(|(drag_rid, _)| drag_rid == rack_id)
                    && slot_act.row_hovered
                {
                    hovered_slot_drop_idx = Some(s_idx);
                }
                slot_rects.push(slot_act.row_rect);

                if app.expanded_sidechain_slots.contains(&slot.id) {
                    param_dirty |= draw_slot_sidechain_drawer(
                        rack_id,
                        slot,
                        &all_rack_names,
                        sc_support,
                        sidechain_ctx.audio_running,
                        ui,
                    );
                }
                if app.expanded_param_slots.contains(&slot.id) {
                    let catalog = app
                        .slot_param_catalog
                        .get(&slot.id)
                        .map_or(&[][..], Vec::as_slice);
                    if let Some(upd) = draw_slot_parameter_drawer(rack_id, slot, catalog, ui) {
                        slot_param_updates.push(upd);
                    }
                }
            }

            // Visual drop line for vertical slot drag & drop reordering
            if let Some((drag_rid, src_idx)) = app.slot_drag_source {
                if drag_rid == rack_id {
                    if let Some(dst_idx) = hovered_slot_drop_idx {
                        if let Some(target_rect) = slot_rects.get(dst_idx) {
                            let y = if src_idx < dst_idx {
                                target_rect.max.y + 1.0
                            } else {
                                target_rect.min.y - 1.0
                            };
                            ui.painter().line_segment(
                                [egui::pos2(target_rect.min.x, y), egui::pos2(target_rect.max.x, y)],
                                Stroke::new(2.5, FL_MINT_GREEN),
                            );
                        }
                        if ui.input(|i| i.pointer.any_released()) {
                            reorder_slot = Some((src_idx, dst_idx));
                            app.slot_drag_source = None;
                        }
                    } else if ui.input(|i| i.pointer.any_released()) {
                        app.slot_drag_source = None;
                    }
                }
            }

            if let Some((src_idx, dst_idx)) = reorder_slot {
                if src_idx != dst_idx && dst_idx < rack.slots.len() {
                    let item = rack.slots.remove(src_idx);
                    rack.slots.insert(dst_idx, item);
                    slot_order_dirty = true;
                }
            } else if let Some((a, b)) = move_swap {
                rack.slots.swap(a, b);
                slot_order_dirty = true;
            }
            if let Some(idx) = remove_slot_idx {
                remove_slot_idx_out = Some(idx);
            }

            ui.add_space(4.0);
            // Single boxed '+' icon button below existing slots (no Slot 2 / Slot 3 placeholders!)
            if draw_boxed_add_plugin_button(ui) {
                app.add_slot_target_rack_id = Some(rack_id);
            }
        });

    ui.add_space(6.0);
    ui.separator();
    ui.add_space(4.0);

    // 3. Bottom Inspector Controls: Width, Gain, Outgoing Cables & Sidechain Send Volumes
    draw_inspector_bottom_controls(
        rack,
        &all_rack_names,
        &outgoing_sidechains,
        &latency_ctx,
        &mut param_dirty,
        &mut bind_req,
        &mut sc_gain_updates,
        &mut inspector_gain_delta,
        ui,
    );

    // Apply deferred actions
    if let Some((src_id, src_gid, delta_db)) = inspector_gain_delta {
        let selected_snapshot = app.selected_rack_ids.clone();
        propagate_grouped_gain_delta(
            &mut app.session.racks,
            &selected_snapshot,
            src_id,
            src_gid,
            delta_db,
        );
    }
    for (r_id, s_id, p_id, val) in slot_param_updates {
        app.set_slot_parameter(r_id, s_id, p_id, val);
    }
    for (dst_rack_id, dst_slot_id, new_db) in sc_gain_updates {
        if let Some(dst_rack) = app.session.racks.iter_mut().find(|r| r.id == dst_rack_id) {
            if let Some(slot) = dst_rack.slots.iter_mut().find(|s| s.id == dst_slot_id) {
                if let Some(ref mut sc) = slot.sidechain {
                    sc.gain_db = new_db;
                    param_dirty = true;
                }
            }
        }
    }
    if remove_rack {
        app.push_undo_checkpoint();
        app.remove_rack_incremental(rack_id);
        app.mark_autosave_dirty();
    } else if let Some(idx) = remove_slot_idx_out {
        app.push_undo_checkpoint();
        app.remove_slot_incremental(rack_id, idx);
        app.mark_autosave_dirty();
    } else if duplicate_rack {
        app.duplicate_rack(rack_id);
    } else if let Some(delta) = move_rack_delta {
        app.move_rack(r_idx, delta);
    } else if let Some(s_idx) = dup_slot_idx {
        app.duplicate_slot(rack_id, s_idx);
    }
    if let Some((r_id, s_id, title)) = open_editor_req {
        app.open_slot_editor(r_id, s_id, &title);
    }
    if let Some(target) = bind_req {
        open_keybindings_window(app, ui.ctx(), Some(target));
    }
    if app.add_slot_target_rack_id.is_some() && app.add_slot_target_rack_id != prev_add_slot_target
    {
        app.focus_plugin_search_box = true;
        if prev_add_slot_target.is_some() {
            ui.ctx()
                .send_viewport_cmd_to(add_plugin_viewport_id(), egui::ViewportCommand::Focus);
        }
    }
    if slot_order_dirty {
        app.sync_slot_order_to_engine(rack_id);
        app.mark_autosave_dirty();
    }
    if param_dirty {
        app.sync_params_to_engine();
        app.mark_autosave_dirty();
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_inspector_header(
    rack: &mut RackConfig,
    rack_index: usize,
    rack_total: usize,
    param_dirty: &mut bool,
    remove_rack: &mut bool,
    duplicate_rack: &mut bool,
    move_rack_delta: &mut Option<isize>,
    bind_req: &mut Option<KeybindTarget>,
    ui: &mut egui::Ui,
) {
    let hdr_resp = egui::Frame::new()
        .fill(BG_PANEL)
        .stroke(Stroke::new(1.0, BORDER_HAIRLINE))
        .corner_radius(CornerRadius::same(2))
        .inner_margin(egui::Margin::symmetric(8, 6))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(format!("R{}", rack_index + 1))
                        .small()
                        .strong()
                        .monospace()
                        .color(FL_MINT_GREEN),
                );
                ui.add(egui::TextEdit::singleline(&mut rack.name).desired_width(f32::INFINITY));
            });
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                let in_label = if rack.receive_input {
                    "Input: LIVE"
                } else {
                    "Input: BUS ONLY"
                };
                *param_dirty |= theme::draw_hardware_toggle(
                    ui,
                    &mut rack.receive_input,
                    in_label,
                    FL_MINT_GREEN,
                    TEXT_ON_ACCENT,
                    108.0,
                )
                .on_hover_text("Toggle live audio input into this strip (off for Aux Bus-only)")
                .changed();

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let mut to_master = rack.route_to_master;
                    let out_label = if to_master {
                        "Out: Master"
                    } else {
                        "Out: Bus"
                    };
                    if theme::draw_hardware_toggle(
                        ui,
                        &mut to_master,
                        out_label,
                        FL_MINT_GREEN,
                        TEXT_ON_ACCENT,
                        84.0,
                    )
                    .on_hover_text("Toggle direct green cable routing to Master (M)")
                    .clicked()
                    {
                        rack.route_to_master = to_master;
                        *param_dirty = true;
                    }
                });
            });
        });

    let rack_id = rack.id;
    hdr_resp.response.context_menu(|ui| {
        ui.menu_button("Strip Color", |ui| {
            for &(label, rgb) in FL_STRIP_PRESET_COLORS {
                if ui.selectable_label(rack.color == rgb, label).clicked() {
                    rack.color = rgb;
                    *param_dirty = true;
                    ui.close();
                }
            }
            ui.separator();
            let mut rgb = rack.color.unwrap_or([64, 128, 160]);
            if ui.color_edit_button_srgb(&mut rgb).changed() {
                rack.color = Some(rgb);
                *param_dirty = true;
            }
        });
        ui.separator();
        if ui
            .add_enabled(rack_index > 0, egui::Button::new("Move Left"))
            .clicked()
        {
            *move_rack_delta = Some(-1);
            ui.close();
        }
        if ui
            .add_enabled(rack_index + 1 < rack_total, egui::Button::new("Move Right"))
            .clicked()
        {
            *move_rack_delta = Some(1);
            ui.close();
        }
        if ui.button("Duplicate Rack").clicked() {
            *duplicate_rack = true;
            ui.close();
        }
        if ui.button("Assign Keybind...").clicked() {
            *bind_req = Some(KeybindTarget::Rack(rack_id));
            ui.close();
        }
        ui.separator();
        if ui
            .button(RichText::new("Delete Rack").color(AUDIO_RED))
            .clicked()
        {
            *remove_rack = true;
            ui.close();
        }
    });
}

struct InspectorSlotAction {
    param_dirty: bool,
    move_up: bool,
    move_down: bool,
    duplicate: bool,
    remove: bool,
    row_rect: Rect,
    row_hovered: bool,
}

#[allow(clippy::too_many_arguments)]
fn draw_inspector_slot_row(
    slot: &mut PluginSlotConfig,
    index: usize,
    total: usize,
    rack_id: Uuid,
    has_editor: bool,
    is_external: bool,
    sc_support: Option<bool>,
    latency: &LatencyDisplayCtx<'_>,
    expanded_param_slots: &mut HashSet<Uuid>,
    expanded_sidechain_slots: &mut HashSet<Uuid>,
    slot_drag_source: &mut Option<(Uuid, usize)>,
    open_editor_req: &mut Option<(Uuid, Uuid, String)>,
    bind_req: &mut Option<KeybindTarget>,
    ui: &mut egui::Ui,
) -> InspectorSlotAction {
    let mut act = InspectorSlotAction {
        param_dirty: false,
        move_up: false,
        move_down: false,
        duplicate: false,
        remove: false,
        row_rect: Rect::NOTHING,
        row_hovered: false,
    };

    let active = slot.enabled && !slot.bypassed;
    let params_open = expanded_param_slots.contains(&slot.id);
    let sc_open = expanded_sidechain_slots.contains(&slot.id);
    let sc_active = slot.sidechain.as_ref().is_some_and(|s| s.enabled);
    let mut child_rclick: Vec<egui::Response> = Vec::new();

    let frame_resp = egui::Frame::new()
        .fill(FL_SLOT_BG)
        .stroke(Stroke::new(1.0, BORDER_SUBTLE))
        .corner_radius(CornerRadius::same(2))
        .inner_margin(egui::Margin::symmetric(6, 4))
        .show(ui, |ui| {
            ui.set_width((ui.available_width() - 4.0).max(220.0));
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;

                // Drag grip + expand chevron
                let (grip_rect, grip_resp) =
                    ui.allocate_exact_size(Vec2::new(14.0, 20.0), Sense::click_and_drag());
                ui.painter().text(
                    grip_rect.center(),
                    egui::Align2::CENTER_CENTER,
                    "::",
                    egui::FontId::monospace(10.0),
                    if grip_resp.hovered() { TEXT_PRIMARY } else { TEXT_MUTED },
                );
                if grip_resp.drag_started() {
                    *slot_drag_source = Some((rack_id, index));
                }
                if grip_resp
                    .on_hover_text("Drag up/down to reorder plugin • Click to expand params")
                    .clicked()
                {
                    if params_open {
                        expanded_param_slots.remove(&slot.id);
                    } else {
                        expanded_param_slots.insert(slot.id);
                    }
                }

                // Slot Name (drag up/down OR click to open GUI/Params OR right-click for menu)
                let label = if slot.preset_name != "Default" && !slot.preset_name.is_empty() {
                    format!("{} ({})", slot.name, slot.preset_name)
                } else {
                    slot.name.clone()
                };
                let name_color = if active { TEXT_PRIMARY } else { TEXT_MUTED };
                let right_controls_w = if sc_active { 138.0 } else { 114.0 };
                let name_w = (ui.available_width() - right_controls_w).max(52.0);
                let (name_rect, name_resp) =
                    ui.allocate_exact_size(Vec2::new(name_w, 20.0), Sense::click_and_drag());
                ui.painter().with_clip_rect(name_rect).text(
                    egui::pos2(name_rect.min.x + 2.0, name_rect.center().y),
                    egui::Align2::LEFT_CENTER,
                    &label,
                    egui::FontId::proportional(12.5),
                    name_color,
                );
                if name_resp.drag_started() {
                    *slot_drag_source = Some((rack_id, index));
                }
                let name_resp = name_resp.on_hover_ui(|ui| {
                    if is_external && has_editor {
                        ui.label(format!("{label}\nClick: Open GUI | Drag: Reorder | Right-click: Menu"));
                    } else {
                        ui.label(format!("{label}\nClick: Toggle params | Drag: Reorder | Right-click: Menu"));
                    }
                });
                if name_resp.clicked() {
                    if is_external && has_editor {
                        *open_editor_req = Some((rack_id, slot.id, slot.name.clone()));
                    } else if params_open {
                        expanded_param_slots.remove(&slot.id);
                    } else {
                        expanded_param_slots.insert(slot.id);
                    }
                }
                child_rclick.push(name_resp);

                // Right-aligned slot controls: GUI / SC (+ SC Send Volume knob) / Wet-Dry Mix Knob / Green LED
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;

                    let led_resp = draw_mint_led_button(
                        ui,
                        active,
                        if active {
                            "Plugin Active (click to disable)"
                        } else {
                            "Plugin Disabled/Bypassed (click to enable)"
                        },
                    );
                    if led_resp.clicked() {
                        if slot.bypassed {
                            slot.bypassed = false;
                            slot.enabled = true;
                        } else {
                            slot.enabled = !slot.enabled;
                        }
                        act.param_dirty = true;
                    }

                    let mut mix_pct = slot.wet_mix * 100.0;
                    if theme::draw_rotary_knob(
                        ui,
                        ui.make_persistent_id(("slot_mix", slot.id)),
                        &mut mix_pct,
                        0.0..=100.0,
                        100.0,
                        20.0,
                        FL_MINT_GREEN,
                        false,
                    )
                    .on_hover_ui(|ui| {
                        ui.label(format!("Wet/Dry Mix: {:.0}% (double-click: 100%)", mix_pct));
                    })
                    .changed()
                    {
                        slot.wet_mix = (mix_pct / 100.0).clamp(0.0, 1.0);
                        act.param_dirty = true;
                    }

                    // Sidechain Send Volume knob directly on slot row when SC is active!
                    if let Some(ref mut sc) = slot.sidechain {
                        if sc.enabled {
                            let sc_db_display = sc.gain_db;
                            if theme::draw_rotary_knob(
                                ui,
                                ui.make_persistent_id(("slot_sc_vol", slot.id)),
                                &mut sc.gain_db,
                                -48.0..=24.0,
                                0.0,
                                18.0,
                                ACCENT_AMBER,
                                false,
                            )
                            .on_hover_ui(|ui| {
                                ui.label(format!(
                                    "Sidechain Input Level: {:+.1} dB (double-click: 0.0 dB)",
                                    sc_db_display
                                ));
                            })
                            .changed()
                            {
                                act.param_dirty = true;
                            }
                        }
                    }

                    let sc_available = sc_support != Some(false) || slot.sidechain.is_some();
                    if sc_available {
                        let sc_lbl = if sc_active { "SC*" } else { "SC" };
                        let sc_color = if sc_open {
                            TEXT_ON_ACCENT
                        } else if sc_active {
                            ACCENT_AMBER
                        } else {
                            TEXT_SECONDARY
                        };
                        let sc_fill = if sc_open {
                            ACCENT_AMBER
                        } else {
                            Color32::from_rgb(36, 40, 46)
                        };
                        if ui
                            .add(
                                egui::Button::new(RichText::new(sc_lbl).small().color(sc_color))
                                    .fill(sc_fill)
                                    .min_size(Vec2::new(26.0, 20.0)),
                            )
                            .on_hover_text("Sidechain key routing & volume")
                            .clicked()
                        {
                            if sc_open {
                                expanded_sidechain_slots.remove(&slot.id);
                            } else {
                                expanded_sidechain_slots.insert(slot.id);
                            }
                        }
                    }

                    if is_external {
                        let gui_btn = egui::Button::new(RichText::new("GUI").small())
                            .min_size(Vec2::new(30.0, 20.0));
                        if ui
                            .add_enabled(has_editor, gui_btn)
                            .on_hover_text("Open native VST3/CLAP editor window")
                            .clicked()
                        {
                            *open_editor_req = Some((rack_id, slot.id, slot.name.clone()));
                        }
                    }
                });
            });

            if params_open {
                ui.add_space(3.0);
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Gain").small().color(TEXT_MUTED));
                    if ui
                        .add(
                            egui::DragValue::new(&mut slot.gain_db)
                                .range(-36.0..=12.0)
                                .speed(0.2)
                                .fixed_decimals(1)
                                .suffix(" dB"),
                        )
                        .changed()
                    {
                        act.param_dirty = true;
                    }
                    let mut byp = slot.bypassed;
                    if theme::draw_hardware_toggle(
                        ui,
                        &mut byp,
                        "Bypass",
                        AUDIO_YELLOW,
                        TEXT_ON_ACCENT,
                        48.0,
                    )
                    .changed()
                    {
                        slot.bypassed = byp;
                        act.param_dirty = true;
                    }
                    if latency.audio_running {
                        if let Some(&smp) = latency.slot_latencies.get(&slot.id) {
                            let ms = format_delay_ms(smp, latency.sample_rate);
                            ui.label(
                                RichText::new(format!("Delay: +{ms:.1} ms"))
                                    .small()
                                    .monospace()
                                    .color(if smp > 0 { ACCENT_AMBER } else { TEXT_MUTED }),
                            );
                        }
                    }
                });
            }
        });

    act.row_rect = frame_resp.response.rect;
    act.row_hovered = ui
        .ctx()
        .pointer_latest_pos()
        .is_some_and(|p| act.row_rect.contains(p));

    // Attach rich right-click context menu on the slot card & name
    frame_resp.response.context_menu(|ui| {
        draw_slot_context_menu(
            slot,
            index,
            total,
            rack_id,
            has_editor,
            is_external,
            sc_support,
            expanded_param_slots,
            expanded_sidechain_slots,
            open_editor_req,
            bind_req,
            &mut act,
            ui,
        );
    });
    for resp in child_rclick {
        resp.context_menu(|ui| {
            draw_slot_context_menu(
                slot,
                index,
                total,
                rack_id,
                has_editor,
                is_external,
                sc_support,
                expanded_param_slots,
                expanded_sidechain_slots,
                open_editor_req,
                bind_req,
                &mut act,
                ui,
            );
        });
    }

    act
}

#[allow(clippy::too_many_arguments)]
fn draw_slot_context_menu(
    slot: &mut PluginSlotConfig,
    index: usize,
    total: usize,
    rack_id: Uuid,
    has_editor: bool,
    is_external: bool,
    sc_support: Option<bool>,
    expanded_param_slots: &mut HashSet<Uuid>,
    expanded_sidechain_slots: &mut HashSet<Uuid>,
    open_editor_req: &mut Option<(Uuid, Uuid, String)>,
    bind_req: &mut Option<KeybindTarget>,
    act: &mut InspectorSlotAction,
    ui: &mut egui::Ui,
) {
    let params_open = expanded_param_slots.contains(&slot.id);
    let sc_open = expanded_sidechain_slots.contains(&slot.id);

    if is_external && has_editor && ui.button("Open Plugin GUI").clicked() {
        *open_editor_req = Some((rack_id, slot.id, slot.name.clone()));
        ui.close();
    }
    if ui.button("Toggle Parameters").clicked() {
        if params_open {
            expanded_param_slots.remove(&slot.id);
        } else {
            expanded_param_slots.insert(slot.id);
        }
        ui.close();
    }
    let sc_available = sc_support != Some(false) || slot.sidechain.is_some();
    if sc_available && ui.button("Toggle Sidechain").clicked() {
        if sc_open {
            expanded_sidechain_slots.remove(&slot.id);
        } else {
            expanded_sidechain_slots.insert(slot.id);
        }
        ui.close();
    }
    ui.separator();
    if ui
        .button(if slot.enabled { "Disable Plugin" } else { "Enable Plugin" })
        .clicked()
    {
        slot.enabled = !slot.enabled;
        act.param_dirty = true;
        ui.close();
    }
    if ui
        .button(if slot.bypassed { "Unbypass" } else { "Bypass" })
        .clicked()
    {
        slot.bypassed = !slot.bypassed;
        act.param_dirty = true;
        ui.close();
    }
    ui.separator();
    if ui.add_enabled(index > 0, egui::Button::new("Move Up")).clicked() {
        act.move_up = true;
        ui.close();
    }
    if ui
        .add_enabled(index + 1 < total, egui::Button::new("Move Down"))
        .clicked()
    {
        act.move_down = true;
        ui.close();
    }
    if ui.button("Duplicate Slot").clicked() {
        act.duplicate = true;
        ui.close();
    }
    if ui.button("Assign Keybind...").clicked() {
        *bind_req = Some(KeybindTarget::Slot(rack_id, slot.id));
        ui.close();
    }
    if (slot.gain_db.abs() > f32::EPSILON || (slot.wet_mix - 1.0).abs() > 0.001)
        && ui.button("Reset Gain & Mix").clicked()
    {
        slot.gain_db = 0.0;
        slot.wet_mix = 1.0;
        act.param_dirty = true;
        ui.close();
    }
    ui.separator();
    if ui
        .button(RichText::new("Remove Plugin").color(AUDIO_RED))
        .clicked()
    {
        act.remove = true;
        ui.close();
    }
}

/// Boxed '+' icon button displayed directly beneath the currently added plugin slots.
fn draw_boxed_add_plugin_button(ui: &mut egui::Ui) -> bool {
    let w = (ui.available_width() - 4.0).max(220.0);
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(w, 28.0), Sense::click());
    if ui.is_rect_visible(rect) {
        let hovered = resp.hovered();
        let fill = if hovered {
            Color32::from_rgb(40, 54, 48)
        } else {
            Color32::from_rgb(30, 33, 38)
        };
        let stroke_color = if hovered { FL_MINT_GREEN } else { BORDER_HAIRLINE };
        let painter = ui.painter();
        painter.rect_filled(rect, 3.0, fill);
        painter.rect_stroke(rect, 3.0, Stroke::new(1.0, stroke_color), StrokeKind::Inside);

        // Centered '+' icon inside a subtle inner square box
        let c = rect.center();
        let box_rect = Rect::from_center_size(c, Vec2::splat(18.0));
        painter.rect_stroke(box_rect, 2.0, Stroke::new(1.0, stroke_color), StrokeKind::Inside);
        let plus_color = if hovered { FL_MINT_GREEN } else { TEXT_SECONDARY };
        painter.line_segment(
            [egui::pos2(c.x - 4.5, c.y), egui::pos2(c.x + 4.5, c.y)],
            Stroke::new(1.8, plus_color),
        );
        painter.line_segment(
            [egui::pos2(c.x, c.y - 4.5), egui::pos2(c.x, c.y + 4.5)],
            Stroke::new(1.8, plus_color),
        );
    }
    resp.on_hover_text("Insert VST3, CLAP, or Built-In plugin").clicked()
}

#[allow(clippy::too_many_arguments)]
fn draw_inspector_bottom_controls(
    rack: &mut RackConfig,
    all_rack_names: &[(Uuid, String)],
    outgoing_sidechains: &[(Uuid, Uuid, String, f32, bool)],
    latency: &LatencyDisplayCtx<'_>,
    param_dirty: &mut bool,
    bind_req: &mut Option<KeybindTarget>,
    sc_gain_updates: &mut Vec<(Uuid, Uuid, f32)>,
    gain_delta: &mut Option<(Uuid, Option<Uuid>, f32)>,
    ui: &mut egui::Ui,
) {
    egui::Frame::new()
        .fill(BG_WELL)
        .stroke(Stroke::new(1.0, BORDER_HAIRLINE))
        .corner_radius(CornerRadius::same(2))
        .inner_margin(8.0)
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new("Gain").small().color(TEXT_MUTED));
                let prev_gain = rack.gain_db;
                if ui
                    .add(
                        egui::DragValue::new(&mut rack.gain_db)
                            .range(-48.0..=12.0)
                            .speed(0.2)
                            .fixed_decimals(1)
                            .suffix(" dB"),
                    )
                    .changed()
                {
                    *param_dirty = true;
                    let delta = rack.gain_db - prev_gain;
                    if delta.abs() >= 1e-4 {
                        *gain_delta = Some((rack.id, rack.group_id, delta));
                    }
                }
                ui.label(RichText::new("Width").small().color(TEXT_MUTED));
                *param_dirty |= ui
                    .add(
                        egui::DragValue::new(&mut rack.width)
                            .range(0.0..=2.0)
                            .speed(0.02)
                            .fixed_decimals(2),
                    )
                    .on_hover_text("M/S Stereo Width (0 = Mono, 1 = Normal, 2 = Wide)")
                    .changed();
            });
            ui.add_space(4.0);
            let (bar_rect, _) =
                ui.allocate_exact_size(Vec2::new(ui.available_width(), 10.0), Sense::hover());
            let mid_y = bar_rect.center().y;
            ui.painter().line_segment(
                [egui::pos2(bar_rect.min.x, mid_y), egui::pos2(bar_rect.max.x, mid_y)],
                Stroke::new(1.0, BORDER_HAIRLINE),
            );
            let half_span = (bar_rect.width() * 0.45) * (rack.width / 2.0).clamp(0.05, 1.0);
            ui.painter().line_segment(
                [
                    egui::pos2(bar_rect.center().x - half_span, mid_y),
                    egui::pos2(bar_rect.center().x + half_span, mid_y),
                ],
                Stroke::new(2.2, FL_MINT_GREEN),
            );
        });

    ui.add_space(4.0);

    // Outgoing Audio Cables (tidy vertical list)
    let mut remove_idx = None;
    let mut add_target = None;
    egui::Frame::new()
        .fill(BG_WELL)
        .stroke(Stroke::new(1.0, BORDER_SUBTLE))
        .corner_radius(CornerRadius::same(2))
        .inner_margin(6.0)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(RichText::new("CABLES").small().strong().color(FL_CABLE_GREEN));
                if all_rack_names.len() > 1 {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        egui::ComboBox::from_id_salt(("inspector_add_send", rack.id))
                            .selected_text("+ Cable")
                            .width(86.0)
                            .show_ui(ui, |ui| {
                                for (id, name) in all_rack_names {
                                    let already = *id == rack.id
                                        || rack.sends.iter().any(|s| s.target_rack_id == *id);
                                    if !already && ui.selectable_label(false, name).clicked() {
                                        add_target = Some(*id);
                                    }
                                }
                            });
                    });
                }
            });

            if rack.sends.is_empty() {
                ui.label(
                    RichText::new("No bus send cables (Master only or click + Cable)")
                        .small()
                        .color(TEXT_MUTED),
                );
            } else {
                ui.add_space(3.0);
                for (idx, send) in rack.sends.iter_mut().enumerate() {
                    let target_name = all_rack_names
                        .iter()
                        .find(|(id, _)| *id == send.target_rack_id)
                        .map(|(_, n)| n.as_str())
                        .unwrap_or("Missing");
                    let row_w = ui.available_width();
                    ui.horizontal(|ui| {
                        ui.set_width(row_w);
                        let left_w = (row_w - 94.0).max(80.0);
                        ui.allocate_ui_with_layout(
                            Vec2::new(left_w, 20.0),
                            egui::Layout::left_to_right(egui::Align::Center),
                            |ui| {
                                let chk = ui
                                    .checkbox(&mut send.enabled, format!("-> {target_name}"))
                                    .on_hover_text("Right-click to assign keybind for this Aux Send");
                                chk.context_menu(|ui| {
                                    if ui.button("Assign Keybind...").clicked() {
                                        *bind_req = Some(KeybindTarget::RackSend {
                                            rack_id: rack.id,
                                            target_rack_id: send.target_rack_id,
                                        });
                                        ui.close();
                                    }
                                });
                                *param_dirty |= chk.changed();
                            },
                        );
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.spacing_mut().item_spacing.x = 4.0;
                            if ui
                                .add(
                                    egui::Button::new(RichText::new("x").small().color(AUDIO_RED))
                                        .min_size(Vec2::new(18.0, 18.0)),
                                )
                                .on_hover_text("Disconnect cable")
                                .clicked()
                            {
                                remove_idx = Some(idx);
                            }
                            *param_dirty |= ui
                                .add_sized(
                                    [66.0, 18.0],
                                    egui::DragValue::new(&mut send.level_db)
                                        .range(-48.0..=6.0)
                                        .speed(0.2)
                                        .fixed_decimals(1)
                                        .suffix(" dB"),
                                )
                                .changed();
                        });
                    });
                }
            }
        });

    if let Some(idx) = remove_idx {
        rack.sends.remove(idx);
        *param_dirty = true;
    }
    if let Some(target_rack_id) = add_target {
        rack.sends.push(RackSendConfig {
            target_rack_id,
            level_db: -6.0,
            enabled: true,
        });
        *param_dirty = true;
    }

    if !outgoing_sidechains.is_empty() {
        ui.add_space(4.0);
        egui::Frame::new()
            .fill(BG_WELL)
            .stroke(Stroke::new(1.0, BORDER_SUBTLE))
            .corner_radius(CornerRadius::same(2))
            .inner_margin(6.0)
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.label(RichText::new("SC SENDS").small().strong().color(ACCENT_AMBER));
                ui.add_space(2.0);
                for &(dst_rack_id, dst_slot_id, ref slot_name, sc_db, _) in outgoing_sidechains {
                    let dst_name = all_rack_names
                        .iter()
                        .find(|(id, _)| *id == dst_rack_id)
                        .map(|(_, n)| n.as_str())
                        .unwrap_or("Rack");
                    let row_w = ui.available_width();
                    ui.horizontal(|ui| {
                        ui.set_width(row_w);
                        let left_w = (row_w - 72.0).max(80.0);
                        ui.allocate_ui_with_layout(
                            Vec2::new(left_w, 20.0),
                            egui::Layout::left_to_right(egui::Align::Center),
                            |ui| {
                                ui.add(
                                    egui::Label::new(
                                        RichText::new(format!("-> {dst_name} ({slot_name})"))
                                            .small()
                                            .color(TEXT_SECONDARY),
                                    )
                                    .truncate(),
                                );
                            },
                        );
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            let mut val = sc_db;
                            if ui
                                .add_sized(
                                    [66.0, 18.0],
                                    egui::DragValue::new(&mut val)
                                        .range(-48.0..=24.0)
                                        .speed(0.2)
                                        .fixed_decimals(1)
                                        .suffix(" dB"),
                                )
                                .on_hover_text("Sidechain key send volume")
                                .changed()
                            {
                                sc_gain_updates.push((dst_rack_id, dst_slot_id, val));
                            }
                        });
                    });
                }
            });
    }

    if latency.audio_running {
        let rack_smp: u32 = rack
            .slots
            .iter()
            .filter(|s| s.enabled && !s.bypassed)
            .filter_map(|s| latency.slot_latencies.get(&s.id).copied())
            .fold(0u32, u32::saturating_add);
        if rack_smp > 0 {
            let ms = format_delay_ms(rack_smp, latency.sample_rate);
            ui.label(
                RichText::new(format!("Strip Delay: +{ms:.1} ms ({rack_smp} smp)"))
                    .small()
                    .monospace()
                    .color(ACCENT_AMBER),
            );
        }
    }
}

fn propagate_grouped_gain_delta(
    racks: &mut [RackConfig],
    selected_ids: &[Uuid],
    src_rack_id: Uuid,
    src_group_id: Option<Uuid>,
    delta_db: f32,
) {
    if delta_db.abs() < 1e-4 {
        return;
    }
    let multi_active = selected_ids.len() > 1 && selected_ids.contains(&src_rack_id);
    for other in racks.iter_mut() {
        if other.id == src_rack_id {
            continue;
        }
        let same_group = src_group_id.is_some() && other.group_id == src_group_id;
        let both_selected = multi_active && selected_ids.contains(&other.id);
        if same_group || both_selected {
            other.gain_db = (other.gain_db + delta_db).clamp(-48.0, 12.0);
        }
    }
}

fn draw_mint_led_button(ui: &mut egui::Ui, active: bool, tooltip: &str) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(ROW_LED_H), Sense::click());
    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        let c = rect.center();
        if active {
            painter.circle_filled(c, 6.2, FL_MINT_DIM);
            painter.circle_filled(c, 4.3, FL_MINT_GREEN);
        } else {
            painter.circle_filled(c, 4.3, Color32::from_rgb(48, 52, 58));
        }
        painter.circle_stroke(c, 4.5, Stroke::new(1.0, Color32::from_rgb(16, 18, 21)));
    }
    resp.on_hover_text(tooltip)
}

fn draw_micro_toggle(
    ui: &mut egui::Ui,
    active: &mut bool,
    label: &str,
    active_bg: Color32,
    active_fg: Color32,
    tooltip: &str,
) -> bool {
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(15.0, ROW_CTRL_H), Sense::click());
    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        let fill = if *active {
            active_bg
        } else if resp.hovered() {
            Color32::from_rgb(46, 50, 58)
        } else {
            Color32::from_rgb(33, 36, 41)
        };
        let border = if *active { active_bg } else { BORDER_SUBTLE };
        let fg = if *active { active_fg } else { TEXT_MUTED };
        painter.rect_filled(rect, 2.0, fill);
        painter.rect_stroke(rect, 2.0, Stroke::new(1.0, border), StrokeKind::Inside);
        painter.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            label,
            egui::FontId::proportional(9.5),
            fg,
        );
    }
    if resp.on_hover_text(tooltip).clicked() {
        *active = !*active;
        return true;
    }
    false
}

fn paint_strip_meter(
    ui: &egui::Ui,
    _rack_id: Uuid,
    rect: Rect,
    peak_l: f32,
    peak_r: f32,
) {
    let painter = ui.painter();
    painter.rect_filled(rect, 1.0, BG_WELL);
    painter.rect_stroke(rect, 1.0, Stroke::new(1.0, BORDER_SUBTLE), StrokeKind::Inside);

    let inner = rect.shrink(1.5);
    let half_w = ((inner.width() - 1.5) * 0.5).max(2.0);
    let l_rect = Rect::from_min_size(inner.min, Vec2::new(half_w, inner.height()));
    let r_rect = Rect::from_min_size(
        egui::pos2(inner.min.x + half_w + 1.5, inner.min.y),
        Vec2::new(half_w, inner.height()),
    );
    for (bar_rect, peak) in [(l_rect, peak_l), (r_rect, peak_r)] {
        let db = audio_core::linear_to_db(peak);
        let norm = ((db + 60.0) / 60.0).clamp(0.0, 1.0);
        if norm > 0.002 {
            let h = bar_rect.height() * norm;
            let fill_rect = Rect::from_min_size(
                egui::pos2(bar_rect.min.x, bar_rect.max.y - h),
                Vec2::new(bar_rect.width(), h),
            );
            let color = if db >= -1.0 {
                AUDIO_RED
            } else if db >= -9.0 {
                AUDIO_YELLOW
            } else {
                FL_MINT_GREEN
            };
            painter.rect_filled(fill_rect, 0.5, color);
        }
    }
}

fn paint_source_routing_plug(painter: &egui::Painter, rect: Rect, hovered: bool) {
    let c = rect.center();
    let color = if hovered { TEXT_PRIMARY } else { FL_MINT_GREEN };
    painter.circle_filled(c, 8.5, FL_MINT_DIM);
    painter.circle_stroke(c, 8.5, Stroke::new(1.2, color));
    painter.line_segment([egui::pos2(c.x, c.y - 4.0), egui::pos2(c.x, c.y + 4.0)], Stroke::new(1.8, color));
    painter.line_segment([egui::pos2(c.x - 3.5, c.y + 0.5), egui::pos2(c.x, c.y + 4.5)], Stroke::new(1.6, color));
    painter.line_segment([egui::pos2(c.x + 3.5, c.y + 0.5), egui::pos2(c.x, c.y + 4.5)], Stroke::new(1.6, color));
}

fn paint_target_routing_socket(
    painter: &egui::Painter,
    rect: Rect,
    connected: bool,
    hovered: bool,
    is_master: bool,
) {
    let c = rect.center();
    if connected {
        painter.circle_filled(c, 9.5, FL_MINT_DIM);
        painter.circle_stroke(
            c,
            9.5,
            Stroke::new(2.0, if hovered { TEXT_PRIMARY } else { FL_CABLE_GREEN }),
        );
        painter.circle_filled(c, 3.5, FL_MINT_GREEN);
    } else {
        let color = if hovered {
            FL_MINT_GREEN
        } else if is_master {
            Color32::from_rgb(94, 128, 112)
        } else {
            Color32::from_rgb(72, 132, 108)
        };
        painter.line_segment([egui::pos2(c.x - 4.0, c.y + 2.0), egui::pos2(c.x, c.y - 3.0)], Stroke::new(1.6, color));
        painter.line_segment([egui::pos2(c.x + 4.0, c.y + 2.0), egui::pos2(c.x, c.y - 3.0)], Stroke::new(1.6, color));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::StageHostApp;
    use session_core::{BuiltInEffectKind, PluginSlotConfig};

    #[test]
    fn fl_mixer_renders_equal_strips_cables_and_dynamic_slots() {
        let mut session = session_core::SessionConfig::default();
        let mut mic = RackConfig::new("Mic");
        mic.color = Some([48, 114, 136]);
        let mut reverb = RackConfig::new("Reverb");
        reverb.receive_input = false;
        reverb.color = Some([120, 58, 58]);
        let rev_id = reverb.id;
        mic.sends.push(RackSendConfig {
            target_rack_id: rev_id,
            level_db: -4.0,
            enabled: true,
        });
        mic.slots.push(PluginSlotConfig::new(
            "3-Band Tone EQ",
            PluginKind::BuiltInEffect(BuiltInEffectKind::ThreeBandEq),
        ));
        let mic_id = mic.id;
        session.racks = vec![mic, reverb];

        let mut app = StageHostApp::new_for_ui_test(session);
        app.selected_rack_id = Some(mic_id);

        let ctx = egui::Context::default();
        let mut last_shapes = Vec::new();
        for _ in 0..2 {
            let input = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(1280.0, 720.0))),
                ..Default::default()
            };
            let mut output = ctx.run_ui(input, |ui| {
                egui::Panel::right("right_fx_inspector")
                    .default_size(340.0)
                    .show(ui, |ui| {
                        draw_right_fx_inspector(&mut app, ui);
                    });
                egui::CentralPanel::default().show(ui, |ui| {
                    draw_center_mixer_strips(&mut app, ui);
                });
            });
            output.textures_delta.clear();
            last_shapes = output.shapes;
        }

        // Collect all strip frame rects (IN, M, Mic, Reverb) and confirm identical dimensions!
        fn collect_tall_rects(shape: &egui::Shape, out: &mut Vec<Rect>) {
            match shape {
                egui::Shape::Rect(r) if r.rect.height() > 200.0 && r.rect.width() < 120.0 => {
                    out.push(r.rect);
                }
                egui::Shape::Vec(items) => {
                    for s in items {
                        collect_tall_rects(s, out);
                    }
                }
                _ => {}
            }
        }
        fn count_beziers(shape: &egui::Shape) -> usize {
            match shape {
                egui::Shape::CubicBezier(_) => 1,
                egui::Shape::Vec(items) => items.iter().map(count_beziers).sum(),
                _ => 0,
            }
        }

        let mut tall_rects = Vec::new();
        let mut bezier_count = 0usize;
        for cs in &last_shapes {
            collect_tall_rects(&cs.shape, &mut tall_rects);
            bezier_count += count_beziers(&cs.shape);
        }
        let strip_rects: Vec<Rect> = tall_rects
            .iter()
            .copied()
            .filter(|r| (r.width() - STRIP_WIDTH).abs() <= 2.5 && r.height() > 300.0)
            .collect();

        assert!(
            strip_rects.len() >= 4,
            "expected IN, M, Mic, Reverb strip rects (~{STRIP_WIDTH}px wide), tall_rects={tall_rects:?}"
        );
        let first_size = strip_rects[0].size();
        for rect in &strip_rects {
            assert!(
                (rect.width() - first_size.x).abs() <= 0.5
                    && (rect.height() - first_size.y).abs() <= 0.5,
                "all mixer strips should have identical size: expected {:?}, got {:?}",
                first_size,
                rect.size()
            );
        }
        assert!(
            bezier_count >= 2,
            "expected >= 2 green Bezier cable shapes, got {bezier_count}"
        );
    }

    #[test]
    fn multi_select_group_and_narrow_toolbar_never_overlap() {
        let mut session = session_core::SessionConfig::default();
        let r1 = RackConfig::new("Vocal");
        let r2 = RackConfig::new("Guitar");
        let r1_id = r1.id;
        let r2_id = r2.id;
        session.racks = vec![r1, r2];

        let mut app = StageHostApp::new_for_ui_test(session);
        app.selected_rack_id = Some(r1_id);
        app.selected_rack_ids = vec![r1_id, r2_id];

        // Group both selected racks into a new Group Bus via StripMutations
        let ctx = egui::Context::default();
        let muts = StripMutations {
            group_selected_to_new_bus: true,
            ..Default::default()
        };
        apply_strip_mutations(&mut app, muts, &ctx);

        assert_eq!(app.session.racks.len(), 3);
        let group_bus = &app.session.racks[2];
        assert!(!group_bus.receive_input);
        assert!(group_bus.route_to_master);
        assert!(!app.session.racks[0].route_to_master);
        assert!(!app.session.racks[1].route_to_master);
        assert_eq!(app.session.racks[0].sends[0].target_rack_id, group_bus.id);
        assert_eq!(app.session.racks[1].sends[0].target_rack_id, group_bus.id);
        assert_eq!(app.session.racks[0].group_id, Some(group_bus.id));
        assert_eq!(app.session.racks[1].group_id, Some(group_bus.id));
        assert_eq!(app.session.racks[2].group_id, None);

        // Changing gain on rack 0 moves rack 1's gain by the same delta, leaving Group Bus at 0.0
        app.session.racks[1].gain_db = -3.0;
        let gid = app.session.racks[0].group_id;
        app.session.racks[0].gain_db = -4.5;
        let gain_muts = StripMutations {
            gain_delta: Some((r1_id, gid, -4.5)),
            ..Default::default()
        };
        apply_strip_mutations(&mut app, gain_muts, &ctx);
        assert!((app.session.racks[1].gain_db - (-7.5)).abs() < 1e-4);
        assert!((app.session.racks[2].gain_db - 0.0).abs() < 1e-4);

        // Verify narrow toolbar renders cleanly without text rect overlapping buttons
        app.expanded_param_slots.insert(Uuid::new_v4());
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(460.0, 200.0))),
            ..Default::default()
        };
        let mut out = ctx.run_ui(input, |ui| {
            egui::CentralPanel::default().show(ui, |ui| {
                draw_mixer_toolbar(&mut app, ui);
            });
        });
        out.textures_delta.clear();

        // Deselecting via empty space clears selection and is not overwritten by ensure_valid_selected_rack
        let deselect_muts = StripMutations {
            deselect_all: true,
            ..Default::default()
        };
        apply_strip_mutations(&mut app, deselect_muts, &ctx);
        assert_eq!(ensure_valid_selected_rack(&mut app), None);
        assert!(app.selected_rack_ids.is_empty());

        // Pending keybind draft commits cleanly and toggles rack.enabled
        app.open_keybindings_for_rack(r1_id);
        app.keybind_draft_action = crate::app::KeybindDraftAction::ToggleRackEnabled;
        app.keybind_draft_combo = session_core::KeyComboSpec {
            ctrl: false,
            alt: false,
            shift: false,
            key: "F8".to_string(),
        };
        assert!(app.commit_keybind_draft());
        assert_eq!(app.session.key_bindings.len(), 1);
        assert!(app.session.racks[0].enabled);
        let action = app.session.key_bindings[0].action.clone();
        app.execute_key_binding_action(&action);
        assert!(!app.session.racks[0].enabled);
    }
}
