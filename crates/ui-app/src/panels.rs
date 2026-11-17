use std::collections::HashMap;
use std::path::PathBuf;

use audio_core::engine::parse_driver_selection;
use audio_core::{format_midi_note, AudioDriverBackend};
use eframe::egui::{self, Color32, CornerRadius, RichText, Stroke, Vec2};
use plugin_host::{PluginFormat, PluginParamInfo};
use session_core::{
    default_app_config_dir, BuiltInEffectKind, InputChannelMode, KeyBindingAction, KeyComboSpec,
    PluginKind, PluginSlotConfig, RackConfig, SidechainConfig, SidechainSourceKind,
};
use uuid::Uuid;

use crate::app::{
    is_builtin_shortcut, is_settings_bundle_path, KeybindDraftAction, PendingSessionAction,
    PluginFilterFormat, PluginFilterRole, SettingsCategory, StageHostApp,
};
use crate::file_dialog;
use crate::theme::{
    self, ACCENT_AMBER, ACCENT_AMBER_DIM, AUDIO_GREEN, AUDIO_RED, AUDIO_RED_ACTIVE,
    AUDIO_RED_DIM, AUDIO_YELLOW, BG_CHASSIS, BG_PANEL, BG_RACK_HEADER, BG_ROW_EVEN, BG_ROW_ODD,
    BG_WELL, BORDER_HAIRLINE, BORDER_SUBTLE, TEXT_MUTED, TEXT_ON_ACCENT, TEXT_PRIMARY,
    TEXT_SECONDARY,
};

fn zebra_row_bg(index: usize) -> Color32 {
    if index.is_multiple_of(2) {
        BG_ROW_EVEN
    } else {
        BG_ROW_ODD
    }
}

pub fn draw_menu_bar(app: &mut StageHostApp, ui: &mut egui::Ui) {
    egui::MenuBar::new().ui(ui, |ui| {
        ui.spacing_mut().item_spacing.x = 16.0;
        ui.spacing_mut().button_padding = Vec2::new(10.0, 4.0);
        ui.menu_button("File", |ui| {
            if ui.button("New Session").clicked() {
                app.request_new_session();
                ui.close();
            }
            if ui.button("Open Session...").clicked() {
                ui.close();
                if let Some(path) = file_dialog::pick_open_file(
                    "Open StageHost Session",
                    "StageHost Session (*.stagehost.json)",
                    "*.stagehost.json;*.json",
                ) {
                    app.request_load_session(path);
                }
            }
            if ui.button("Save Session (Ctrl+S)").clicked() {
                app.save_session_to_disk();
                ui.close();
            }
            if ui.button("Save Session As...").clicked() {
                ui.close();
                if let Some(path) = file_dialog::pick_save_file(
                    "Save StageHost Session As",
                    "session.stagehost.json",
                    "StageHost Session (*.stagehost.json)",
                    "*.stagehost.json",
                ) {
                    app.session_path_input = path.display().to_string();
                    app.save_session_to_disk();
                }
            }
            ui.separator();
            if ui.button("Load Set List...").clicked() {
                ui.close();
                if let Some(path) = file_dialog::pick_open_file(
                    "Open StageHost Set List",
                    "StageHost Set List (*.stagelist.json)",
                    "*.stagelist.json;*.json",
                ) {
                    app.setlist_path_input = path.display().to_string();
                    app.load_setlist_from_disk();
                }
            }
            if ui.button("Save Set List").clicked() {
                app.save_setlist_to_disk();
                ui.close();
            }
            if ui.button("Save Set List As...").clicked() {
                ui.close();
                if let Some(path) = file_dialog::pick_save_file(
                    "Save StageHost Set List As",
                    "setlist.stagelist.json",
                    "StageHost Set List (*.stagelist.json)",
                    "*.stagelist.json",
                ) {
                    app.setlist_path_input = path.display().to_string();
                    app.save_setlist_to_disk();
                }
            }
            ui.separator();
            if ui.button("Save All Settings to File...").clicked() {
                ui.close();
                if let Some(path) = file_dialog::pick_save_file(
                    "Save All StageHost Settings",
                    "stagehost.stagehost-settings.json",
                    "StageHost All Settings (*.stagehost-settings.json)",
                    "*.stagehost-settings.json",
                ) {
                    app.export_all_settings_file(&path);
                }
            }
            if ui.button("Load All Settings from File...").clicked() {
                ui.close();
                if let Some(path) = file_dialog::pick_open_file(
                    "Load All StageHost Settings",
                    "StageHost All Settings (*.stagehost-settings.json)",
                    "*.stagehost-settings.json;*.json",
                ) {
                    app.import_all_settings_file(&path);
                }
            }
            ui.separator();
            if ui.button("Minimize to System Tray").clicked() {
                app.request_hide_to_tray(ui.ctx());
                ui.close();
            }
            if ui.button("Exit StageHost RS").clicked() {
                app.request_exit_app(ui.ctx());
                ui.close();
            }
        });

        ui.menu_button("Edit", |ui| {
            if ui
                .add_enabled(app.history.can_undo(), egui::Button::new("Undo (Ctrl+Z)"))
                .clicked()
            {
                app.undo();
                ui.close();
            }
            if ui
                .add_enabled(app.history.can_redo(), egui::Button::new("Redo (Ctrl+Y)"))
                .clicked()
            {
                app.redo();
                ui.close();
            }
        });

        ui.menu_button("View", |ui| {
            if ui
                .checkbox(&mut app.show_left_pane, "Session List & Sub-Sessions Pane")
                .changed()
            {
                app.save_preferences();
                ui.close();
            }
            if ui
                .checkbox(&mut app.show_io_strip, "FX Rack Inspector Panel")
                .changed()
            {
                app.save_preferences();
                ui.close();
            }
            ui.separator();
            if ui
                .add_enabled(
                    !app.expanded_param_slots.is_empty() || !app.expanded_sidechain_slots.is_empty(),
                    egui::Button::new("Collapse All Slot Parameters"),
                )
                .clicked()
            {
                app.expanded_param_slots.clear();
                app.expanded_sidechain_slots.clear();
                ui.close();
            }
        });

        ui.menu_button("Audio", |ui| {
            let running = app.audio_engine.is_running();
            let engine_label = if running {
                "Stop Audio Engine"
            } else {
                "Start Audio Engine"
            };
            if ui.button(engine_label).clicked() {
                if running {
                    app.stop_audio();
                } else {
                    app.start_audio();
                }
                ui.close();
            }
            if ui
                .add_enabled(running, egui::Button::new("Restart Audio Engine"))
                .clicked()
            {
                app.restart_audio();
                ui.close();
            }
            ui.separator();
            if ui
                .add_enabled(running, egui::Button::new("Panic (Reset DSP Buffers)"))
                .clicked()
            {
                app.panic_audio();
                ui.close();
            }
            if ui.button("Audio & Plugin Options...").clicked() {
                app.refresh_devices();
                open_options_window(app, ui.ctx(), Some(SettingsCategory::AudioEngine));
                ui.close();
            }
        });

        ui.menu_button("Racks", |ui| {
            if ui.button("Add Empty Rack").clicked() {
                app.push_undo_checkpoint();
                let next_idx = app.session.racks.len() + 1;
                let rack = RackConfig::new(format!("Insert {next_idx}"));
                let id = rack.id;
                app.add_rack_incremental(rack);
                app.selected_rack_id = Some(id);
                app.mark_autosave_dirty();
                ui.close();
            }
            if ui.button("Scan VST3 / CLAP Plugins...").clicked() {
                app.start_plugin_scan();
                ui.close();
            }
            if ui.button("Plugin Folders & Options...").clicked() {
                open_options_window(app, ui.ctx(), Some(SettingsCategory::Plugins));
                ui.close();
            }
        });

        ui.menu_button("Bindings", |ui| {
            if ui.button("Key Bindings Manager...").clicked() {
                open_keybindings_window(app, ui.ctx(), None);
                ui.close();
            }
            if ui
                .add_enabled(
                    !app.session.key_bindings.is_empty(),
                    egui::Button::new("Clear All Key Bindings"),
                )
                .clicked()
            {
                app.push_undo_checkpoint();
                app.session.key_bindings.clear();
                app.set_status("Cleared all key bindings", false);
                ui.close();
            }
        });

        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.add(
                egui::Label::new(
                    RichText::new("PgUp/PgDn: Song | Alt+1..9: Sub-Session")
                        .small()
                        .color(TEXT_MUTED),
                )
                .truncate(),
            );
            ui.separator();
            if app.is_session_dirty() {
                ui.label(RichText::new("Modified *").small().color(AUDIO_YELLOW));
            } else {
                ui.label(RichText::new("Saved").small().color(TEXT_MUTED));
            }
        });
    });
}

pub fn draw_top_transport_bar(app: &mut StageHostApp, ui: &mut egui::Ui) {
    ui.horizontal(|ui| {
        draw_engine_controls(app, ui);
        ui.separator();
        draw_stage_display(app, ui);

        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            draw_quick_monitor_controls(app, ui);
            if app.tuner_enabled {
                ui.separator();
                draw_tuner_readout(app, ui);
            }
        });
    });
}

fn draw_engine_controls(app: &mut StageHostApp, ui: &mut egui::Ui) {
    let running = app.audio_engine.is_running();
    theme::draw_led_indicator(ui, running, AUDIO_GREEN);
    let engine_text = if running { "Engine ON" } else { "Engine OFF" };
    let engine_btn = if running {
        egui::Button::new(RichText::new(engine_text).strong().color(TEXT_PRIMARY))
            .fill(Color32::from_rgb(38, 60, 46))
            .stroke(Stroke::new(1.0, AUDIO_GREEN))
    } else {
        egui::Button::new(RichText::new(engine_text).strong().color(AUDIO_YELLOW))
            .fill(Color32::from_rgb(56, 46, 26))
            .stroke(Stroke::new(1.0, AUDIO_YELLOW))
    };
    if ui.add(engine_btn).clicked() {
        if running {
            app.stop_audio();
        } else {
            app.start_audio();
        }
    }

    if ui
        .add_enabled(
            running,
            egui::Button::new(RichText::new("Panic").color(AUDIO_YELLOW)),
        )
        .on_hover_text("Clear plugin tails and reset DSP buffers")
        .clicked()
    {
        app.panic_audio();
    }

    let mut tuner_active = app.tuner_enabled;
    if ui
        .add_enabled(
            running,
            egui::Button::new(if tuner_active {
                RichText::new("Tuner").strong().color(TEXT_ON_ACCENT)
            } else {
                RichText::new("Tuner").color(TEXT_SECONDARY)
            })
            .fill(if tuner_active {
                ACCENT_AMBER
            } else {
                Color32::from_rgb(44, 47, 54)
            }),
        )
        .on_hover_text("Toggle real-time chromatic tuner on input channel")
        .clicked()
    {
        tuner_active = !tuner_active;
        app.set_tuner_enabled(tuner_active);
    }

    if ui.button("Setup").on_hover_text("Audio & Plugin Host Settings").clicked() {
        app.refresh_devices();
        open_options_window(app, ui.ctx(), None);
    }

    let kb_count = app.session.key_bindings.len();
    let kb_label = if kb_count > 0 {
        format!("Keys ({kb_count})")
    } else {
        "Keys".to_string()
    };
    if ui
        .button(kb_label)
        .on_hover_text("Configure keyboard shortcuts for VST/CLAP plugins and racks")
        .clicked()
    {
        open_keybindings_window(app, ui.ctx(), None);
    }
}

fn draw_stage_display(app: &mut StageHostApp, ui: &mut egui::Ui) {
    let has_setlist = !app.set_list.entries.is_empty();
    if ui
        .add_enabled(has_setlist, egui::Button::new("<").min_size(Vec2::new(24.0, 26.0)))
        .on_hover_text("Previous Song (PageUp)")
        .clicked()
    {
        app.step_setlist(-1);
    }

    let active_file = app.active_session_config_filename();

    egui::Frame::new()
        .fill(BG_WELL)
        .stroke(Stroke::new(1.0, BORDER_HAIRLINE))
        .corner_radius(CornerRadius::same(2))
        .inner_margin(egui::Margin::symmetric(12, 3))
        .show(ui, |ui| {
            ui.label(
                RichText::new(active_file)
                    .size(14.5)
                    .strong()
                    .color(TEXT_PRIMARY),
            );
        });

    if ui
        .add_enabled(has_setlist, egui::Button::new(">").min_size(Vec2::new(24.0, 26.0)))
        .on_hover_text("Next Song (PageDown)")
        .clicked()
    {
        app.step_setlist(1);
    }

    if app.is_session_dirty()
        && ui
            .button(RichText::new("Save *").color(AUDIO_YELLOW))
            .on_hover_text("Save current session (Ctrl+S)")
            .clicked()
    {
        app.save_session_to_disk();
    }
}

fn draw_tuner_readout(app: &StageHostApp, ui: &mut egui::Ui) {
    let reading = app.telemetry.tuner;
    egui::Frame::new()
        .fill(BG_WELL)
        .stroke(Stroke::new(1.0, BORDER_HAIRLINE))
        .corner_radius(CornerRadius::same(2))
        .inner_margin(egui::Margin::symmetric(8, 3))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                if reading.active {
                    let note = format_midi_note(reading.midi_note);
                    let color = if reading.cents.abs() <= 5.0 {
                        AUDIO_GREEN
                    } else {
                        ACCENT_AMBER
                    };
                    ui.label(RichText::new(note).strong().monospace().color(color));
                    theme::draw_tuner_deviation_bar(ui, reading.cents, true);
                    ui.label(
                        RichText::new(format!("{:+.0}c ({:.1} Hz)", reading.cents, reading.freq_hz))
                            .small()
                            .monospace()
                            .color(TEXT_SECONDARY),
                    );
                } else {
                    ui.label(RichText::new("--").small().monospace().color(TEXT_MUTED));
                    theme::draw_tuner_deviation_bar(ui, 0.0, false);
                }
            });
        });
}

fn draw_quick_monitor_controls(app: &mut StageHostApp, ui: &mut egui::Ui) {
    let engine_running = app.audio_engine.is_running();
    let mut mon_muted = app.output_muted;
    let (mon_label, off_color) = if mon_muted {
        ("Mon: MUTE", TEXT_PRIMARY)
    } else if engine_running {
        ("Mon: ON", TEXT_PRIMARY)
    } else {
        ("Mon: OFF", TEXT_MUTED)
    };
    let hover = if !engine_running && !mon_muted {
        "Audio engine is stopped (no sound). Start Engine to activate Monitor Out, or click to pre-mute."
    } else {
        "Mute speaker output while keeping Input, Rack meters, and Tuner active"
    };
    if theme::draw_hardware_toggle(
        ui,
        &mut mon_muted,
        mon_label,
        AUDIO_RED_ACTIVE,
        off_color,
        74.0,
    )
    .on_hover_text(hover)
    .clicked()
    {
        app.set_output_muted(mon_muted);
    }

    let mut in_muted = !app.input_enabled;
    let in_label = if app.input_enabled { "In: ON" } else { "In: MUTE" };
    if theme::draw_hardware_toggle(
        ui,
        &mut in_muted,
        in_label,
        AUDIO_RED_DIM,
        AUDIO_RED,
        64.0,
    )
    .on_hover_text("Mute live audio input capture before racks and input meter")
    .clicked()
    {
        app.input_enabled = !in_muted;
        app.sync_input_config_to_engine();
        app.save_preferences();
    }

    if !app.show_io_strip {
        theme::draw_stereo_meter(
            ui,
            app.telemetry.master_peak_l,
            app.telemetry.master_peak_r,
            64.0,
        );
        theme::draw_stereo_meter(
            ui,
            app.telemetry.input_peak_l,
            app.telemetry.input_peak_r,
            52.0,
        );
    }
}

pub fn draw_bottom_status_bar(app: &StageHostApp, ui: &mut egui::Ui) {
    ui.horizontal(|ui| {
        let (msg, is_error) = app
            .status_banner
            .as_ref()
            .map(|(m, e)| (m.as_str(), *e))
            .unwrap_or(("Ready", false));
        let led_color = if is_error {
            AUDIO_RED
        } else if app.audio_engine.is_running() {
            AUDIO_GREEN
        } else {
            TEXT_MUTED
        };
        theme::draw_led_indicator(ui, is_error || app.audio_engine.is_running(), led_color);
        ui.label(
            RichText::new(msg)
                .small()
                .color(if is_error { AUDIO_RED } else { TEXT_SECONDARY }),
        );

        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let t = &app.telemetry;
            let cpu_color = if t.cpu_load_pct > 80.0 {
                AUDIO_RED
            } else if t.cpu_load_pct > 55.0 {
                AUDIO_YELLOW
            } else {
                TEXT_SECONDARY
            };
            ui.label(
                RichText::new(format!("XRuns: {}", t.xrun_count))
                    .small()
                    .monospace()
                    .color(if t.xrun_count > 0 {
                        AUDIO_YELLOW
                    } else {
                        TEXT_MUTED
                    }),
            );
            ui.separator();
            ui.label(
                RichText::new(format!("CPU {:.1}%", t.cpu_load_pct))
                    .small()
                    .monospace()
                    .color(cpu_color),
            );
            ui.separator();
            ui.label(
                RichText::new(format!("{} Hz / {} smp", t.sample_rate, t.buffer_frames))
                    .small()
                    .monospace()
                    .color(TEXT_SECONDARY),
            );
            ui.separator();
            let sr_safe = if t.sample_rate > 0 {
                t.sample_rate
            } else {
                app.selected_sample_rate.max(8_000)
            };
            let buf_frames = if app.audio_engine.is_running() && t.buffer_frames > 0 {
                t.buffer_frames
            } else {
                app.selected_buffer_frames
            };
            let buf_ms = format_delay_ms(buf_frames, sr_safe);
            let fx_smp = if app.audio_engine.is_running() {
                t.plugin_latency_samples
            } else {
                0
            };
            let fx_ms = format_delay_ms(fx_smp, sr_safe);
            let total_ms = buf_ms + fx_ms;
            let delay_label = if fx_smp > 0 {
                format!("Delay: {total_ms:.1} ms (Buf {buf_ms:.1} + FX {fx_ms:.1})")
            } else {
                format!("Delay: {buf_ms:.1} ms")
            };
            let delay_color = if fx_smp > 0 {
                ACCENT_AMBER
            } else {
                TEXT_SECONDARY
            };
            ui.label(
                RichText::new(delay_label)
                    .small()
                    .monospace()
                    .color(delay_color),
            )
            .on_hover_ui(move |ui| {
                ui.label(format!(
                    "Audio buffer: {buf_frames} smp ({buf_ms:.2} ms)\nActive plugin chain delay: {fx_smp} smp ({fx_ms:.2} ms)\nTotal buffer + plugin delay: {total_ms:.2} ms"
                ));
            });
            ui.separator();
            let out_name = if app.audio_engine.is_running() {
                app.audio_engine.active_device_name()
            } else {
                "Audio Stopped"
            };
            ui.label(RichText::new(out_name).small().color(TEXT_MUTED));
        });
    });
}

pub fn draw_left_sidebar(app: &mut StageHostApp, ui: &mut egui::Ui) {
    egui::ScrollArea::vertical().show(ui, |ui| {
        draw_session_list_section(app, ui);
        ui.add_space(10.0);
        draw_sub_session_section(app, ui);
    });
}

fn draw_section_header(ui: &mut egui::Ui, title: &str, hint: Option<&str>) {
    egui::Frame::new()
        .fill(BG_RACK_HEADER)
        .stroke(Stroke::new(1.0, BORDER_HAIRLINE))
        .corner_radius(CornerRadius::same(2))
        .inner_margin(egui::Margin::symmetric(8, 4))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new(title).small().strong().color(TEXT_PRIMARY));
                if let Some(h) = hint {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(RichText::new(h).small().color(TEXT_MUTED));
                    });
                }
            });
        });
}

fn draw_sub_session_section(app: &mut StageHostApp, ui: &mut egui::Ui) {
    draw_section_header(ui, "SUB-SESSIONS", Some("Alt+1..9"));

    let rename_key = ui.make_persistent_id("renaming_sub_session_id");
    let mut renaming_id = ui
        .ctx()
        .data(|d| d.get_temp::<Option<Uuid>>(rename_key).flatten());
    let mut activate_id: Option<Uuid> = None;
    let mut update_id: Option<Uuid> = None;
    let mut remove_id: Option<Uuid> = None;
    let total_subs = app.session.sub_sessions.len();

    egui::Frame::new()
        .fill(BG_WELL)
        .stroke(Stroke::new(1.0, BORDER_HAIRLINE))
        .corner_radius(CornerRadius::same(2))
        .inner_margin(4.0)
        .show(ui, |ui| {
            for (idx, sub) in app.session.sub_sessions.iter_mut().enumerate() {
                let is_active = app.session.active_sub_session == Some(sub.id);
                let is_renaming = renaming_id == Some(sub.id);
                let row_fill = if is_active {
                    ACCENT_AMBER_DIM
                } else {
                    zebra_row_bg(idx)
                };
                egui::Frame::new()
                    .fill(row_fill)
                    .stroke(if is_active {
                        Stroke::new(1.0, ACCENT_AMBER)
                    } else {
                        Stroke::NONE
                    })
                    .inner_margin(egui::Margin::symmetric(4, 2))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            if is_renaming {
                                let edit = ui.add(
                                    egui::TextEdit::singleline(&mut sub.name).desired_width(110.0),
                                );
                                if edit.lost_focus()
                                    && ui.input(|i| i.key_pressed(egui::Key::Enter))
                                {
                                    renaming_id = None;
                                }
                                if ui.small_button("Ok").clicked() {
                                    renaming_id = None;
                                }
                            } else {
                                let label = format!("{}. {}", idx + 1, sub.name);
                                let text = if is_active {
                                    RichText::new(label).strong().color(ACCENT_AMBER)
                                } else {
                                    RichText::new(label).color(TEXT_PRIMARY)
                                };
                                if ui
                                    .add_sized([124.0, 20.0], egui::Button::new(text).frame(false))
                                    .clicked()
                                {
                                    activate_id = Some(sub.id);
                                }
                                if ui
                                    .small_button("Ren")
                                    .on_hover_text("Rename sub-session")
                                    .clicked()
                                {
                                    renaming_id = Some(sub.id);
                                }
                            }
                            if ui
                                .small_button("Upd")
                                .on_hover_text("Overwrite snapshot with current rack states")
                                .clicked()
                            {
                                update_id = Some(sub.id);
                            }
                            if total_subs > 1
                                && ui
                                    .small_button("x")
                                    .on_hover_text("Delete sub-session")
                                    .clicked()
                            {
                                remove_id = Some(sub.id);
                            }
                        });
                    });
            }
        });

    ui.ctx().data_mut(|d| d.insert_temp(rename_key, renaming_id));

    if let Some(id) = activate_id {
        app.activate_sub_session(id);
    }
    if let Some(id) = update_id {
        app.push_undo_checkpoint();
        if let Err(err) = app.session.update_sub_session(id) {
            app.set_status(format!("Update sub-session failed: {err}"), true);
        } else {
            app.set_status("Updated active Sub-Session snapshot", false);
        }
    }
    if let Some(id) = remove_id {
        app.push_undo_checkpoint();
        app.session.sub_sessions.retain(|s| s.id != id);
        if app.session.active_sub_session == Some(id) {
            app.session.active_sub_session = app.session.sub_sessions.first().map(|s| s.id);
        }
    }

    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.add(
            egui::TextEdit::singleline(&mut app.new_sub_session_name)
                .hint_text("Part name (e.g. Chorus)")
                .desired_width(142.0),
        );
        if ui.button("+ Capture").clicked() {
            app.push_undo_checkpoint();
            let name = if app.new_sub_session_name.trim().is_empty() {
                format!("Part {}", app.session.sub_sessions.len() + 1)
            } else {
                app.new_sub_session_name.trim().to_string()
            };
            let snap = app.session.capture_sub_session(name);
            let id = snap.id;
            app.session.sub_sessions.push(snap);
            app.session.active_sub_session = Some(id);
            app.new_sub_session_name.clear();
        }
    });
}

fn pick_and_add_session_file(app: &mut StageHostApp) {
    if let Some(path) = file_dialog::pick_open_file(
        "Add Saved Session / Settings File",
        "StageHost Presets (*.stagehost-settings.json, *.stagehost.json)",
        "*.stagehost-settings.json;*.stagehost.json;*.json",
    ) {
        app.add_session_file_to_list(path);
    }
}

fn draw_session_list_general_menu(app: &mut StageHostApp, ui: &mut egui::Ui) {
    if ui.button("Add File from Explorer...").clicked() {
        pick_and_add_session_file(app);
        ui.close();
    }
    if ui.button("Save Current Setup as Preset...").clicked() {
        let default_file = if app.session.name.trim().is_empty() {
            "preset.stagehost-settings.json".to_string()
        } else {
            let safe_name: String = app
                .session
                .name
                .trim()
                .chars()
                .map(|c| {
                    if c.is_alphanumeric() || c == '-' || c == '_' {
                        c
                    } else {
                        '_'
                    }
                })
                .collect();
            format!("{safe_name}.stagehost-settings.json")
        };
        if let Some(path) = file_dialog::pick_save_file(
            "Save Settings Preset to Session List",
            &default_file,
            "StageHost All Settings (*.stagehost-settings.json)",
            "*.stagehost-settings.json",
        ) {
            app.save_preset_and_add_to_session_list(path);
        }
        ui.close();
    }
    if ui.button("Add Current Active File").clicked() {
        app.add_current_session_to_setlist();
        ui.close();
    }
    ui.separator();
    if ui.button("Save Session List...").clicked() {
        if let Some(path) = file_dialog::pick_save_file(
            "Save StageHost Session List",
            "session_list.stagelist.json",
            "StageHost Set List (*.stagelist.json)",
            "*.stagelist.json",
        ) {
            app.setlist_path_input = path.display().to_string();
            app.save_setlist_to_disk();
        }
        ui.close();
    }
    if ui.button("Load Session List...").clicked() {
        if let Some(path) = file_dialog::pick_open_file(
            "Open StageHost Session List",
            "StageHost Set List (*.stagelist.json)",
            "*.stagelist.json;*.json",
        ) {
            app.setlist_path_input = path.display().to_string();
            app.load_setlist_from_disk();
        }
        ui.close();
    }
}

fn draw_session_list_section(app: &mut StageHostApp, ui: &mut egui::Ui) {
    let header_resp = egui::Frame::new()
        .fill(BG_RACK_HEADER)
        .stroke(Stroke::new(1.0, BORDER_HAIRLINE))
        .corner_radius(CornerRadius::same(2))
        .inner_margin(egui::Margin::symmetric(8, 4))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new("SESSION LIST")
                        .small()
                        .strong()
                        .color(TEXT_PRIMARY),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .add_sized([22.0, 18.0], egui::Button::new(RichText::new("+").strong()))
                        .on_hover_text("Add saved session/settings file from Windows Explorer")
                        .clicked()
                    {
                        pick_and_add_session_file(app);
                    }
                });
            });
        });
    header_resp.response.context_menu(|ui| {
        draw_session_list_general_menu(app, ui);
    });

    ui.add_space(4.0);

    let mut load_entry: Option<Uuid> = None;
    let mut keybind_entry: Option<Uuid> = None;
    let mut remove_entry: Option<Uuid> = None;
    let mut move_swap: Option<(usize, usize)> = None;
    let total_entries = app.set_list.entries.len();

    let list_frame = egui::Frame::new()
        .fill(BG_WELL)
        .stroke(Stroke::new(1.0, BORDER_HAIRLINE))
        .corner_radius(CornerRadius::same(2))
        .inner_margin(4.0)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            if app.set_list.entries.is_empty() {
                ui.add_space(6.0);
                ui.add(
                    egui::Label::new(
                        RichText::new("No session files yet. Click '+' to add from Explorer.")
                            .small()
                            .color(TEXT_MUTED),
                    )
                    .wrap(),
                );
                ui.add_space(6.0);
                return;
            }

            for (idx, entry) in app.set_list.entries.iter().enumerate() {
                let is_active = app.active_setlist_entry_id == Some(entry.id);
                let row_fill = if is_active {
                    ACCENT_AMBER_DIM
                } else {
                    zebra_row_bg(idx)
                };
                let matching_combo = app
                    .session
                    .key_bindings
                    .iter()
                    .find(|b| {
                        b.enabled
                            && matches!(
                                &b.action,
                                KeyBindingAction::LoadSessionFile { entry_id, path }
                                if *entry_id == entry.id
                                    || (!path.as_os_str().is_empty() && *path == entry.session_path)
                            )
                    })
                    .map(|b| b.combo.clone());

                let row_resp = egui::Frame::new()
                    .fill(row_fill)
                    .stroke(if is_active {
                        Stroke::new(1.0, ACCENT_AMBER)
                    } else {
                        Stroke::NONE
                    })
                    .inner_margin(egui::Margin::symmetric(6, 4))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        let label_text = format!("{}. {}", idx + 1, entry.title);
                        let rich_label = if is_active {
                            RichText::new(label_text).strong().color(ACCENT_AMBER)
                        } else {
                            RichText::new(label_text).color(TEXT_PRIMARY)
                        };

                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 6.0;
                            let max_name_w = if matching_combo.is_some() {
                                (ui.available_width() - 62.0).max(60.0)
                            } else {
                                ui.available_width()
                            };
                            ui.scope(|ui| {
                                ui.set_max_width(max_name_w);
                                ui.add(egui::Label::new(rich_label).truncate());
                            });
                            if let Some(ref combo) = matching_combo {
                                ui.label(
                                    RichText::new(format!("[{}]", combo.display_label()))
                                        .small()
                                        .monospace()
                                        .color(ACCENT_AMBER),
                                );
                            }
                        });
                    })
                    .response
                    .interact(egui::Sense::click())
                    .on_hover_ui(|ui| {
                        ui.label(format!(
                            "Click to switch | Right-click for Keybind / options\n{}",
                            entry.session_path.display()
                        ));
                    });

                if row_resp.clicked() {
                    load_entry = Some(entry.id);
                }
                row_resp.context_menu(|ui| {
                    ui.label(RichText::new(&entry.title).small().strong().color(ACCENT_AMBER));
                    ui.separator();
                    if ui.button("Switch to This Session").clicked() {
                        load_entry = Some(entry.id);
                        ui.close();
                    }
                    if ui.button("Set Keybind...").clicked() {
                        keybind_entry = Some(entry.id);
                        ui.close();
                    }
                    if idx > 0 && ui.button("Move Up").clicked() {
                        move_swap = Some((idx, idx - 1));
                        ui.close();
                    }
                    if idx + 1 < total_entries && ui.button("Move Down").clicked() {
                        move_swap = Some((idx, idx + 1));
                        ui.close();
                    }
                    ui.separator();
                    if ui.button("Remove from Session List").clicked() {
                        remove_entry = Some(entry.id);
                        ui.close();
                    }
                });
            }
        });

    list_frame.response.context_menu(|ui| {
        draw_session_list_general_menu(app, ui);
    });

    if let Some((a, b)) = move_swap {
        app.set_list.entries.swap(a, b);
        app.save_autosave_workspace();
    }
    if let Some(id) = load_entry {
        app.request_load_setlist_entry(id);
    }
    if let Some(id) = keybind_entry {
        app.open_keybindings_for_session_entry(id);
    }
    if let Some(id) = remove_entry {
        app.remove_session_list_entry(id);
    }
}

pub(crate) fn input_mode_label(mode: InputChannelMode) -> &'static str {
    match mode {
        InputChannelMode::Stereo => "ST",
        InputChannelMode::MonoLeft => "L->ST",
        InputChannelMode::MonoRight => "R->ST",
        InputChannelMode::MonoSum => "SUM",
    }
}

pub(crate) fn next_input_mode(mode: InputChannelMode) -> InputChannelMode {
    match mode {
        InputChannelMode::Stereo => InputChannelMode::MonoLeft,
        InputChannelMode::MonoLeft => InputChannelMode::MonoRight,
        InputChannelMode::MonoRight => InputChannelMode::MonoSum,
        InputChannelMode::MonoSum => InputChannelMode::Stereo,
    }
}

pub(crate) struct LatencyDisplayCtx<'a> {
    pub(crate) audio_running: bool,
    pub(crate) sample_rate: u32,
    pub(crate) slot_latencies: &'a HashMap<Uuid, u32>,
}

pub(crate) struct SidechainDisplayCtx<'a> {
    pub(crate) audio_running: bool,
    pub(crate) slot_caps: &'a HashMap<Uuid, bool>,
    pub(crate) scanned_plugins: &'a [plugin_host::PluginDescriptor],
}

impl SidechainDisplayCtx<'_> {
    pub(crate) fn slot_support(&self, slot: &PluginSlotConfig) -> Option<bool> {
        StageHostApp::resolve_slot_sidechain_support(self.slot_caps, self.scanned_plugins, slot)
    }
}

pub(crate) fn format_delay_ms(samples: u32, sample_rate: u32) -> f32 {
    if sample_rate == 0 {
        0.0
    } else {
        (samples as f32 * 1000.0) / (sample_rate as f32)
    }
}

pub(crate) fn format_sidechain_source(
    source: &SidechainSourceKind,
    all_rack_names: &[(Uuid, String)],
) -> String {
    match source {
        SidechainSourceKind::Rack { rack_id, pre_fader } => {
            let rack_name = all_rack_names
                .iter()
                .find(|(id, _)| id == rack_id)
                .map(|(_, n)| n.as_str())
                .unwrap_or("Missing Rack");
            let tap = if *pre_fader { "Pre" } else { "Post" };
            format!("Rack: {rack_name} ({tap})")
        }
        SidechainSourceKind::LiveInput { mode } => match mode {
            InputChannelMode::Stereo => "Live Input (Stereo)".to_string(),
            InputChannelMode::MonoLeft => "Live Input (Ch 1 / L)".to_string(),
            InputChannelMode::MonoRight => "Live Input (Ch 2 / R)".to_string(),
            InputChannelMode::MonoSum => "Live Input (Mono Sum)".to_string(),
        },
    }
}

pub(crate) fn draw_slot_sidechain_drawer(
    rack_id: Uuid,
    slot: &mut PluginSlotConfig,
    all_rack_names: &[(Uuid, String)],
    sc_support: Option<bool>,
    audio_running: bool,
    ui: &mut egui::Ui,
) -> bool {
    let mut dirty = false;
    let mut clear_sc = false;
    let mut selected_source: Option<SidechainSourceKind> =
        slot.sidechain.as_ref().map(|s| s.source);

    egui::Frame::new()
        .fill(BG_CHASSIS)
        .stroke(Stroke::new(1.0, BORDER_HAIRLINE))
        .inner_margin(8.0)
        .show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.label(
                    RichText::new("SIDECHAIN INPUT:")
                        .small()
                        .strong()
                        .color(ACCENT_AMBER),
                );
                if let Some(ref mut sc) = slot.sidechain {
                    dirty |= ui.checkbox(&mut sc.enabled, "Active").changed();
                }

                let current_label = match slot.sidechain {
                    Some(ref sc) => format_sidechain_source(&sc.source, all_rack_names),
                    None => "None (Internal)".to_string(),
                };
                let current_pre_fader = matches!(
                    slot.sidechain,
                    Some(SidechainConfig {
                        source: SidechainSourceKind::Rack { pre_fader: true, .. },
                        ..
                    })
                );

                egui::ComboBox::from_id_salt(("slot_sc_src", slot.id))
                    .selected_text(current_label)
                    .width(195.0)
                    .show_ui(ui, |ui| {
                        if ui
                            .selectable_label(slot.sidechain.is_none(), "None (Internal)")
                            .clicked()
                        {
                            clear_sc = true;
                        }
                        ui.separator();
                        for (mode, label) in [
                            (InputChannelMode::Stereo, "Live Input (Stereo 1+2)"),
                            (InputChannelMode::MonoLeft, "Live Input (Ch 1 / Left)"),
                            (InputChannelMode::MonoRight, "Live Input (Ch 2 / Right)"),
                            (InputChannelMode::MonoSum, "Live Input (Mono Sum)"),
                        ] {
                            let cand = SidechainSourceKind::LiveInput { mode };
                            if ui
                                .selectable_label(selected_source == Some(cand), label)
                                .clicked()
                            {
                                selected_source = Some(cand);
                                dirty = true;
                            }
                        }
                        let other_racks: Vec<_> = all_rack_names
                            .iter()
                            .filter(|(id, _)| *id != rack_id)
                            .collect();
                        if !other_racks.is_empty() {
                            ui.separator();
                            for &(id, ref name) in other_racks {
                                let is_cur = matches!(
                                    selected_source,
                                    Some(SidechainSourceKind::Rack { rack_id: rid, .. }) if rid == id
                                );
                                if ui
                                    .selectable_label(is_cur, format!("Rack: {name}"))
                                    .clicked()
                                {
                                    selected_source = Some(SidechainSourceKind::Rack {
                                        rack_id: id,
                                        pre_fader: current_pre_fader,
                                    });
                                    dirty = true;
                                }
                            }
                        }
                    });

                if clear_sc {
                    slot.sidechain = None;
                    dirty = true;
                } else if let Some(new_src) = selected_source {
                    match slot.sidechain {
                        Some(ref mut sc) if sc.source != new_src => {
                            sc.source = new_src;
                            sc.enabled = true;
                        }
                        None => {
                            slot.sidechain = Some(SidechainConfig {
                                enabled: true,
                                source: new_src,
                                gain_db: 0.0,
                            });
                        }
                        _ => {}
                    }
                }

                if let Some(ref mut sc) = slot.sidechain {
                    if let SidechainSourceKind::Rack {
                        ref mut pre_fader, ..
                    } = sc.source
                    {
                        dirty |= theme::draw_hardware_toggle(
                            ui,
                            pre_fader,
                            "Pre-Fader",
                            ACCENT_AMBER,
                            TEXT_ON_ACCENT,
                            68.0,
                        )
                        .on_hover_text(
                            "Tap source rack output before its Gain/Pan/Width fader",
                        )
                        .changed();
                    }

                    ui.label(RichText::new("Trim").small().color(TEXT_MUTED));
                    dirty |= ui
                        .add(
                            egui::DragValue::new(&mut sc.gain_db)
                                .range(-36.0..=24.0)
                                .speed(0.2)
                                .fixed_decimals(1)
                                .suffix(" dB"),
                        )
                        .on_hover_text("Sidechain key input trim")
                        .changed();

                    if ui
                        .small_button("Clear")
                        .on_hover_text("Disconnect sidechain input")
                        .clicked()
                    {
                        slot.sidechain = None;
                        dirty = true;
                    }
                }

                if sc_support == Some(false) {
                    ui.label(
                        RichText::new("Note: this plugin reported no auxiliary/sidechain input bus.")
                            .small()
                            .color(AUDIO_YELLOW),
                    );
                } else if sc_support.is_none() && !audio_running {
                    ui.label(
                        RichText::new("(Start audio engine to verify VST3/CLAP aux input bus)")
                            .small()
                            .color(TEXT_MUTED),
                    );
                }
            });
        });

    dirty
}

pub(crate) fn draw_slot_parameter_drawer(
    rack_id: Uuid,
    slot: &mut PluginSlotConfig,
    catalog: &[PluginParamInfo],
    ui: &mut egui::Ui,
) -> Option<(Uuid, Uuid, u32, f32)> {
    let mut changed_param = None;
    let filter_id = ui.make_persistent_id(("slot_param_filter", slot.id));
    egui::Frame::new()
        .fill(BG_CHASSIS)
        .stroke(Stroke::new(1.0, BORDER_HAIRLINE))
        .inner_margin(8.0)
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new("Preset:").small().color(TEXT_MUTED));
                ui.add(egui::TextEdit::singleline(&mut slot.preset_name).desired_width(140.0));
                if catalog.len() > 6 {
                    let mut filter = ui
                        .ctx()
                        .data_mut(|d| d.get_temp::<String>(filter_id).unwrap_or_default());
                    ui.separator();
                    ui.label(RichText::new("Filter:").small().color(TEXT_MUTED));
                    ui.add(
                        egui::TextEdit::singleline(&mut filter)
                            .desired_width(120.0)
                            .hint_text("Param name..."),
                    );
                    if !filter.is_empty() && ui.small_button("Clear").clicked() {
                        filter.clear();
                    }
                    ui.ctx().data_mut(|d| d.insert_temp(filter_id, filter));
                }
            });
            ui.add_space(4.0);

            if catalog.is_empty() {
                ui.label(
                    RichText::new("No host-automatable parameters reported by this plugin.")
                        .small()
                        .color(TEXT_MUTED),
                );
                return;
            }
            let filter_lower = if catalog.len() > 6 {
                ui.ctx()
                    .data(|d| d.get_temp::<String>(filter_id).unwrap_or_default())
                    .trim()
                    .to_ascii_lowercase()
            } else {
                String::new()
            };

            egui::Grid::new(ui.scope_id().with(("slot_params", slot.id)))
                .num_columns(2)
                .spacing([12.0, 4.0])
                .show(ui, |ui| {
                    for info in catalog {
                        if !filter_lower.is_empty()
                            && !info.name.to_ascii_lowercase().contains(&filter_lower)
                        {
                            continue;
                        }
                        ui.label(RichText::new(&info.name).small().color(TEXT_SECONDARY));
                        let mut val = *slot.params.get(&info.id).unwrap_or(&info.value);
                        let resp = ui.add(
                            egui::Slider::new(&mut val, info.min..=info.max).fixed_decimals(3),
                        );
                        if resp.changed() {
                            slot.params.insert(info.id, val);
                            changed_param = Some((rack_id, slot.id, info.id, val));
                        }
                        ui.end_row();
                    }
                });
        });
    changed_param
}

pub fn draw_modals(app: &mut StageHostApp, ctx: &egui::Context) {
    draw_settings_modal(app, ctx);
    draw_keybindings_modal(app, ctx);
    draw_add_plugin_modal(app, ctx);
    draw_unsaved_confirm_modal(app, ctx);
}

fn unsaved_confirm_viewport_id() -> egui::ViewportId {
    egui::ViewportId::from_hash_of("stagehost_unsaved_confirm_viewport")
}

pub(crate) fn add_plugin_viewport_id() -> egui::ViewportId {
    egui::ViewportId::from_hash_of("stagehost_add_plugin_viewport")
}

pub enum KeybindTarget {
    Rack(Uuid),
    MultiRack(Vec<Uuid>),
    Slot(Uuid, Uuid),
    RackSend { rack_id: Uuid, target_rack_id: Uuid },
}

fn keybindings_viewport_id() -> egui::ViewportId {
    egui::ViewportId::from_hash_of("stagehost_keybindings_viewport")
}

pub fn open_keybindings_window(
    app: &mut StageHostApp,
    ctx: &egui::Context,
    target: Option<KeybindTarget>,
) {
    let was_open = app.show_keybindings;
    match target {
        Some(KeybindTarget::Rack(rack_id)) => app.open_keybindings_for_rack(rack_id),
        Some(KeybindTarget::MultiRack(rack_ids)) => app.open_keybindings_for_multi_rack(rack_ids),
        Some(KeybindTarget::Slot(rack_id, slot_id)) => {
            app.open_keybindings_for_slot(rack_id, slot_id)
        }
        Some(KeybindTarget::RackSend {
            rack_id,
            target_rack_id,
        }) => app.open_keybindings_for_rack_send(rack_id, target_rack_id),
        None => app.open_keybindings_modal(),
    }
    if was_open {
        ctx.send_viewport_cmd_to(keybindings_viewport_id(), egui::ViewportCommand::Focus);
    }
}

fn draw_unsaved_confirm_modal(app: &mut StageHostApp, ctx: &egui::Context) {
    let Some(ref pending) = app.pending_session_action else {
        return;
    };
    let action_desc = match pending {
        PendingSessionAction::NewSession => "starting a new session",
        PendingSessionAction::LoadSession(_) => "loading another session",
        PendingSessionAction::LoadSetListEntry(_) => "switching Session List preset",
    };
    let prompt_text = format!(
        "Session '{}' has unsaved changes before {action_desc}.",
        app.session.name
    );
    let mut keep_open = true;
    let mut choice: Option<bool> = None;

    let builder = egui::ViewportBuilder::default()
        .with_title("Unsaved Session Changes - StageHost")
        .with_inner_size([460.0, 155.0])
        .with_min_inner_size([380.0, 135.0])
        .with_resizable(false)
        .with_minimize_button(false)
        .with_maximize_button(false);

    ctx.show_viewport_immediate(unsaved_confirm_viewport_id(), builder, |ui, _class| {
        if ui.ctx().input(|i| {
            i.viewport().close_requested() || i.key_pressed(egui::Key::Escape)
        }) {
            keep_open = false;
        }

        egui::Panel::bottom("unsaved_confirm_footer")
            .frame(
                egui::Frame::new()
                    .fill(BG_PANEL)
                    .stroke(Stroke::new(1.0, BORDER_HAIRLINE))
                    .inner_margin(egui::Margin::symmetric(12, 8)),
            )
            .show(ui, |ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Cancel").clicked() {
                        keep_open = false;
                    }
                    if ui
                        .button(RichText::new("Discard & Continue").color(AUDIO_YELLOW))
                        .clicked()
                    {
                        choice = Some(false);
                    }
                    if ui
                        .button(RichText::new("Save & Continue").strong().color(ACCENT_AMBER))
                        .clicked()
                    {
                        choice = Some(true);
                    }
                });
            });

        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(BG_CHASSIS)
                    .inner_margin(egui::Margin::same(14)),
            )
            .show(ui, |ui| {
                ui.add(
                    egui::Label::new(RichText::new(&prompt_text).color(TEXT_PRIMARY)).wrap(),
                );
            });
    });

    if !keep_open {
        app.pending_session_action = None;
    } else if let Some(save_first) = choice {
        app.resolve_pending_session_action(save_first);
    }
}

fn options_viewport_id() -> egui::ViewportId {
    egui::ViewportId::from_hash_of("stagehost_options_viewport")
}

pub fn open_options_window(
    app: &mut StageHostApp,
    ctx: &egui::Context,
    category: Option<SettingsCategory>,
) {
    if let Some(cat) = category {
        app.settings_category = cat;
    }
    let was_open = app.show_settings;
    app.show_settings = true;
    if !was_open && app.cached_driver_notes.is_none() {
        app.refresh_selected_driver_notes();
    }
    if was_open {
        ctx.send_viewport_cmd_to(options_viewport_id(), egui::ViewportCommand::Focus);
    }
}

fn draw_settings_modal(app: &mut StageHostApp, ctx: &egui::Context) {
    if !app.show_settings {
        return;
    }

    let builder = egui::ViewportBuilder::default()
        .with_title("Options - StageHost")
        .with_inner_size([840.0, 540.0])
        .with_min_inner_size([720.0, 440.0]);

    ctx.show_viewport_immediate(options_viewport_id(), builder, |ui, _class| {
        if ui.ctx().input(|i| {
            i.viewport().close_requested() || i.key_pressed(egui::Key::Escape)
        }) {
            app.show_settings = false;
        }

        egui::Panel::bottom("options_footer_panel")
            .frame(
                egui::Frame::new()
                    .fill(BG_PANEL)
                    .stroke(Stroke::new(1.0, BORDER_HAIRLINE))
                    .inner_margin(egui::Margin::symmetric(12, 8)),
            )
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    let footer_w = ui.available_width();
                    let btn_w = 96.0;
                    ui.allocate_ui_with_layout(
                        Vec2::new((footer_w - btn_w - 8.0).max(160.0), 24.0),
                        egui::Layout::left_to_right(egui::Align::Center),
                        |ui| {
                            ui.add(
                                egui::Label::new(
                                    RichText::new("Settings auto-save to AppData/preferences.json")
                                        .small()
                                        .color(TEXT_MUTED),
                                )
                                .truncate(),
                            );
                        },
                    );
                    ui.allocate_ui_with_layout(
                        Vec2::new(btn_w, 24.0),
                        egui::Layout::right_to_left(egui::Align::Center),
                        |ui| {
                            if ui
                                .add_sized([84.0, 24.0], egui::Button::new("Close"))
                                .clicked()
                            {
                                app.show_settings = false;
                            }
                        },
                    );
                });
            });

        egui::Panel::left("options_sidebar_panel")
            .resizable(false)
            .default_size(188.0)
            .size_range(176.0..=210.0)
            .frame(
                egui::Frame::new()
                    .fill(BG_PANEL)
                    .stroke(Stroke::new(1.0, BORDER_HAIRLINE))
                    .inner_margin(egui::Margin::symmetric(8, 10)),
            )
            .show(ui, |ui| {
                for (cat, label) in [
                    (SettingsCategory::AudioEngine, "Audio Engine"),
                    (SettingsCategory::Plugins, "VST / CLAP Plugins"),
                    (SettingsCategory::General, "General & Session"),
                    (SettingsCategory::Workspace, "Workspace & Keys"),
                ] {
                    if draw_settings_nav_item(ui, label, app.settings_category == cat) {
                        app.settings_category = cat;
                    }
                    ui.add_space(4.0);
                }
            });

        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(BG_CHASSIS)
                    .inner_margin(egui::Margin::same(14)),
            )
            .show(ui, |ui| {
                egui::ScrollArea::vertical()
                    .id_salt("options_right_pane")
                    .auto_shrink([false, false])
                    .show(ui, |ui| match app.settings_category {
                        SettingsCategory::AudioEngine => draw_settings_audio_page(app, ui),
                        SettingsCategory::Plugins => draw_settings_plugins_page(app, ui),
                        SettingsCategory::General => draw_settings_general_page(app, ui),
                        SettingsCategory::Workspace => draw_settings_workspace_page(app, ui),
                    });
            });
    });
}

fn draw_settings_nav_item(ui: &mut egui::Ui, label: &str, selected: bool) -> bool {
    let fill = if selected {
        BG_RACK_HEADER
    } else {
        Color32::TRANSPARENT
    };
    let stroke = if selected {
        Stroke::new(1.0, ACCENT_AMBER)
    } else {
        Stroke::NONE
    };
    let text_col = if selected { ACCENT_AMBER } else { TEXT_PRIMARY };
    let row_w = ui.available_width().max(120.0);
    let btn = egui::Button::new(RichText::new(label).color(text_col).strong())
        .fill(fill)
        .stroke(stroke)
        .corner_radius(CornerRadius::same(2));
    ui.add_sized([row_w, 28.0], btn).clicked()
}

fn draw_settings_page_banner(ui: &mut egui::Ui, title: &str) {
    egui::Frame::new()
        .fill(BG_RACK_HEADER)
        .stroke(Stroke::new(1.0, BORDER_SUBTLE))
        .corner_radius(CornerRadius::same(2))
        .inner_margin(egui::Margin::symmetric(8, 5))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.add(egui::Label::new(RichText::new(title).strong().color(TEXT_PRIMARY)).wrap());
        });
    ui.add_space(10.0);
}

fn draw_setting_row_custom(
    ui: &mut egui::Ui,
    label_width: f32,
    label: &str,
    label_color: Color32,
    bold_label: bool,
    add_contents: impl FnOnce(&mut egui::Ui),
) {
    ui.horizontal_top(|ui| {
        ui.allocate_ui_with_layout(
            Vec2::new(label_width, 20.0),
            egui::Layout::left_to_right(egui::Align::Center),
            |ui| {
                let mut rt = RichText::new(label).color(label_color);
                if bold_label {
                    rt = rt.strong();
                }
                ui.add(egui::Label::new(rt).wrap());
            },
        );
        ui.add_space(8.0);
        let content_w = ui.available_width().max(180.0);
        ui.allocate_ui_with_layout(
            Vec2::new(content_w, 20.0),
            egui::Layout::top_down(egui::Align::Min),
            |ui| {
                ui.set_width(content_w);
                add_contents(ui);
            },
        );
    });
    ui.add_space(6.0);
}

fn draw_setting_row(ui: &mut egui::Ui, label: &str, add_contents: impl FnOnce(&mut egui::Ui)) {
    draw_setting_row_custom(ui, 152.0, label, TEXT_SECONDARY, false, add_contents);
}

fn format_sample_rate_khz(sr: u32) -> String {
    format!("{}.{:03}Hz", sr / 1000, sr % 1000)
}

fn draw_driver_notes_panel(app: &mut StageHostApp, ui: &mut egui::Ui) {
    if !app.show_driver_notes {
        return;
    }
    egui::Frame::new()
        .fill(BG_WELL)
        .stroke(Stroke::new(1.0, BORDER_SUBTLE))
        .corner_radius(CornerRadius::same(2))
        .inner_margin(egui::Margin::same(8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width().clamp(220.0, 420.0));
            if let Some(ref err) = app.driver_notes_error {
                ui.label(RichText::new(err).small().color(AUDIO_RED));
            } else if let Some(ref notes) = app.cached_driver_notes {
                ui.label(RichText::new(&notes.title).small().strong().color(TEXT_PRIMARY));
                ui.add_space(3.0);
                for line in &notes.summary_lines {
                    ui.add(egui::Label::new(RichText::new(line).small().color(TEXT_SECONDARY)).wrap());
                }
            } else {
                ui.label(RichText::new("No driver details loaded.").small().color(TEXT_MUTED));
            }
        });
    ui.add_space(6.0);
}

fn format_asio_channel_pair_label(
    start: usize,
    total_channels: usize,
    channel_names: &[String],
) -> String {
    let end = (start + 2).min(total_channels);
    let base = if start + 1 < total_channels {
        format!("Ch {}-{}", start + 1, end)
    } else {
        format!("Ch {} (Mono)", start + 1)
    };
    let n0 = channel_names.get(start).map(String::as_str).unwrap_or("").trim();
    let n1 = if start + 1 < total_channels {
        channel_names.get(start + 1).map(String::as_str).unwrap_or("").trim()
    } else {
        ""
    };
    match (n0.is_empty(), n1.is_empty()) {
        (false, false) => format!("{base} ({n0} / {n1})"),
        (false, true) => format!("{base} ({n0})"),
        (true, false) => format!("{base} ({n1})"),
        (true, true) => base,
    }
}

fn draw_asio_channel_pair_row(
    ui: &mut egui::Ui,
    label: &str,
    id_salt: &str,
    total_channels: usize,
    channel_names: &[String],
    offset: &mut usize,
    changed: &mut bool,
) {
    if total_channels <= 2 {
        return;
    }
    let combo_w = ui.available_width().clamp(220.0, 340.0);
    draw_setting_row(ui, label, |ui| {
        let cur_label = format_asio_channel_pair_label(*offset, total_channels, channel_names);
        egui::ComboBox::from_id_salt(id_salt)
            .width(combo_w)
            .selected_text(cur_label)
            .show_ui(ui, |ui| {
                let mut start = 0usize;
                while start < total_channels {
                    let text =
                        format_asio_channel_pair_label(start, total_channels, channel_names);
                    if ui.selectable_label(*offset == start, text).clicked() {
                        *offset = start;
                        *changed = true;
                    }
                    start += 2;
                }
            });
    });
}

fn draw_settings_audio_page(app: &mut StageHostApp, ui: &mut egui::Ui) {
    draw_settings_page_banner(ui, "Audio Driver");

    let (selected_backend, _) = parse_driver_selection(app.selected_audio_device.as_deref());
    let driver_combo_w = ui.available_width().clamp(220.0, 400.0);
    let ctrl_combo_w = ui.available_width().clamp(160.0, 220.0);

    draw_setting_row(ui, "Audio Driver:", |ui| {
        let dev_label = app
            .selected_audio_device
            .as_deref()
            .unwrap_or("WASAPI - <System Default>")
            .to_string();
        let mut selection_changed = false;
        egui::ComboBox::from_id_salt("audio_dev_combo")
            .width(driver_combo_w)
            .selected_text(dev_label)
            .show_ui(ui, |ui| {
                let mut shown_wasapi_sep = false;
                let has_asio = app
                    .available_audio_devices
                    .iter()
                    .any(|d| d.backend == AudioDriverBackend::Asio);

                for dev in &app.available_audio_devices {
                    if dev.backend == AudioDriverBackend::Wasapi && has_asio && !shown_wasapi_sep {
                        ui.separator();
                        shown_wasapi_sep = true;
                    }
                    let selected =
                        app.selected_audio_device.as_deref() == Some(dev.id_string.as_str());
                    if ui.selectable_label(selected, &dev.id_string).clicked() {
                        app.selected_audio_device = Some(dev.id_string.clone());
                        selection_changed = true;
                    }
                }

                if shown_wasapi_sep || !has_asio {
                    let is_sys_default = app.selected_audio_device.is_none();
                    if ui
                        .selectable_label(is_sys_default, "WASAPI - <System Default>")
                        .clicked()
                    {
                        app.selected_audio_device = None;
                        selection_changed = true;
                    }
                }
            });
        if selection_changed {
            app.cached_driver_notes = None;
            app.driver_notes_error = None;
            if app.show_driver_notes || parse_driver_selection(app.selected_audio_device.as_deref()).0 == AudioDriverBackend::Asio {
                app.refresh_selected_driver_notes();
                if let Some(ref notes) = app.cached_driver_notes {
                    if !notes.supported_sample_rates.is_empty()
                        && !notes.supported_sample_rates.contains(&app.selected_sample_rate)
                    {
                        app.selected_sample_rate = if notes.supported_sample_rates.contains(&48_000)
                        {
                            48_000
                        } else if notes.supported_sample_rates.contains(&44_100) {
                            44_100
                        } else {
                            notes.supported_sample_rates[0]
                        };
                    }
                }
            }
            app.save_preferences();
        }
    });

    draw_setting_row(ui, "", |ui| {
        ui.horizontal_wrapped(|ui| {
            let cp_btn = egui::Button::new("Control Panel").min_size(Vec2::new(168.0, 22.0));
            if ui.add(cp_btn).clicked() {
                if let Err(err) = app
                    .audio_engine
                    .open_control_panel(app.selected_audio_device.as_deref())
                {
                    app.set_status(format!("Control Panel error: {err}"), true);
                }
            }
            ui.add_space(6.0);
            let link_color = Color32::from_rgb(0x52, 0x9C, 0xE2);
            let notes_resp = ui.add(
                egui::Button::new(RichText::new("Notes about this driver").color(link_color))
                    .frame(false),
            );
            if notes_resp.clicked() {
                app.show_driver_notes = !app.show_driver_notes;
                if app.show_driver_notes {
                    app.refresh_selected_driver_notes();
                }
            }
        });
    });

    if app.show_driver_notes {
        draw_setting_row(ui, "", |ui| {
            draw_driver_notes_panel(app, ui);
        });
    }

    let rate_options: Vec<u32> = app
        .cached_driver_notes
        .as_ref()
        .filter(|n| {
            app.selected_audio_device
                .as_deref()
                .is_none_or(|sel| n.title == sel)
                && !n.supported_sample_rates.is_empty()
        })
        .map(|n| n.supported_sample_rates.clone())
        .unwrap_or_else(|| vec![44_100, 48_000, 88_200, 96_000, 176_400, 192_000]);

    draw_setting_row(ui, "Sample Rate:", |ui| {
        let mut sr_changed = false;
        egui::ComboBox::from_id_salt("audio_sr_combo")
            .width(ctrl_combo_w)
            .selected_text(format_sample_rate_khz(app.selected_sample_rate))
            .show_ui(ui, |ui| {
                for &sr in &rate_options {
                    let selected = app.selected_sample_rate == sr;
                    if ui
                        .selectable_label(selected, format_sample_rate_khz(sr))
                        .clicked()
                    {
                        app.selected_sample_rate = sr;
                        sr_changed = true;
                    }
                }
            });
        if sr_changed {
            app.save_preferences();
        }
    });

    let mut buffer_options = vec![64u32, 128, 256, 512, 1024, 2048];
    if let Some(ref notes) = app.cached_driver_notes {
        if notes.preferred_buffer_size > 0 && !buffer_options.contains(&notes.preferred_buffer_size)
        {
            buffer_options.push(notes.preferred_buffer_size);
            buffer_options.sort_unstable();
        }
    }
    if !buffer_options.contains(&app.selected_buffer_frames) {
        buffer_options.push(app.selected_buffer_frames);
        buffer_options.sort_unstable();
    }

    draw_setting_row(ui, "Buffer Size:", |ui| {
        let mut buf_changed = false;
        ui.horizontal_wrapped(|ui| {
            egui::ComboBox::from_id_salt("audio_buf_combo")
                .width(ctrl_combo_w)
                .selected_text(format!("{}", app.selected_buffer_frames))
                .show_ui(ui, |ui| {
                    for &buf in &buffer_options {
                        let selected = app.selected_buffer_frames == buf;
                        if ui.selectable_label(selected, format!("{buf}")).clicked() {
                            app.selected_buffer_frames = buf;
                            buf_changed = true;
                        }
                    }
                });
            let latency_ms = (app.selected_buffer_frames as f32
                / app.selected_sample_rate.max(1) as f32)
                * 1000.0;
            ui.label(
                RichText::new(format!("({latency_ms:.1} ms)"))
                    .small()
                    .color(TEXT_MUTED),
            );
        });
        if buf_changed {
            app.save_preferences();
        }
    });

    if selected_backend == AudioDriverBackend::Asio {
        if let Some(notes) = app
            .cached_driver_notes
            .as_ref()
            .filter(|n| n.backend == AudioDriverBackend::Asio)
            .cloned()
        {
            let mut ch_changed = false;
            draw_asio_channel_pair_row(
                ui,
                "ASIO Input Pair:",
                "asio_in_pair_combo",
                notes.input_channels,
                &notes.input_channel_names,
                &mut app.asio_input_channel,
                &mut ch_changed,
            );
            draw_asio_channel_pair_row(
                ui,
                "ASIO Output Pair:",
                "asio_out_pair_combo",
                notes.output_channels,
                &notes.output_channel_names,
                &mut app.asio_output_channel,
                &mut ch_changed,
            );
            if ch_changed {
                app.save_preferences();
            }
        }
    } else {
        draw_setting_row(ui, "Input Device:", |ui| {
            if app.available_input_devices.is_empty() {
                ui.add(
                    egui::Label::new(
                        RichText::new("No capture device found (output-only)").color(TEXT_MUTED),
                    )
                    .wrap(),
                );
            } else {
                let in_label = app
                    .selected_input_device
                    .as_deref()
                    .unwrap_or("<System Default>")
                    .to_string();
                let mut in_changed = false;
                egui::ComboBox::from_id_salt("audio_in_dev_combo")
                    .width(driver_combo_w)
                    .selected_text(in_label)
                    .show_ui(ui, |ui| {
                        let is_default = app.selected_input_device.is_none();
                        if ui.selectable_label(is_default, "<System Default>").clicked() {
                            app.selected_input_device = None;
                            in_changed = true;
                        }
                        for dev in &app.available_input_devices {
                            let selected =
                                app.selected_input_device.as_deref() == Some(dev.name.as_str());
                            if ui.selectable_label(selected, &dev.name).clicked() {
                                app.selected_input_device = Some(dev.name.clone());
                                in_changed = true;
                            }
                        }
                    });
                if in_changed {
                    app.save_preferences();
                }
            }
        });
    }

    ui.add_space(4.0);
    ui.horizontal_wrapped(|ui| {
        if ui
            .button(RichText::new("Apply & Restart Engine").strong())
            .clicked()
        {
            app.save_preferences();
            app.start_audio();
        }
        if ui.button("Refresh Devices").clicked() {
            app.refresh_devices();
        }
        let running = app.audio_engine.is_running();
        let status_col = if running { AUDIO_GREEN } else { TEXT_MUTED };
        let status_text = if running {
            format!(
                "Running: {} Hz / {} frames",
                app.audio_engine.active_sample_rate(),
                app.audio_engine.active_buffer_frames()
            )
        } else {
            "Engine stopped".to_string()
        };
        ui.label(RichText::new(status_text).small().color(status_col));
    });

    if !app.audio_engine.is_running() {
        if let Some((ref msg, true)) = app.status_banner {
            ui.add_space(4.0);
            ui.add(egui::Label::new(RichText::new(msg).small().color(AUDIO_RED)).wrap());
        } else if selected_backend == AudioDriverBackend::Asio {
            if let Some(ref err) = app.driver_notes_error {
                ui.add_space(4.0);
                ui.add(egui::Label::new(RichText::new(err).small().color(AUDIO_RED)).wrap());
            }
        }
    }

    ui.add_space(14.0);
    draw_settings_page_banner(ui, "Input Channel & Monitor Routing");

    draw_setting_row(ui, "Input Capture:", |ui| {
        if ui
            .checkbox(&mut app.input_enabled, "Enable audio input routing into racks")
            .changed()
        {
            app.sync_input_config_to_engine();
            app.save_preferences();
        }
    });

    draw_setting_row(ui, "Input Channel Mode:", |ui| {
        ui.horizontal_wrapped(|ui| {
            for (mode, label) in [
                (InputChannelMode::Stereo, "Stereo"),
                (InputChannelMode::MonoLeft, "Mono Left -> ST"),
                (InputChannelMode::MonoRight, "Mono Right -> ST"),
                (InputChannelMode::MonoSum, "Mono Sum"),
            ] {
                if ui
                    .selectable_value(&mut app.input_mode, mode, label)
                    .changed()
                {
                    app.sync_input_config_to_engine();
                    app.save_preferences();
                }
            }
        });
    });

    draw_setting_row(ui, "Input Gain:", |ui| {
        let in_resp = ui.add(
            egui::Slider::new(&mut app.input_gain_db, -36.0..=18.0)
                .suffix(" dB")
                .fixed_decimals(1),
        );
        if in_resp.changed() {
            app.sync_input_config_to_engine();
        }
        if in_resp.drag_stopped() {
            app.save_preferences();
        }
    });

    draw_setting_row(ui, "Monitor Output:", |ui| {
        let mut muted = app.output_muted;
        if ui
            .checkbox(
                &mut muted,
                "Mute speaker monitor output (input & rack meters remain active)",
            )
            .changed()
        {
            app.set_output_muted(muted);
        }
    });

    draw_setting_row(ui, "Master Output Gain:", |ui| {
        let master_resp = ui.add(
            egui::Slider::new(&mut app.session.master_gain_db, -60.0..=12.0)
                .suffix(" dB")
                .fixed_decimals(1),
        );
        if master_resp.changed() {
            app.sync_master_gain_to_engine();
        }
    });

    draw_setting_row(ui, "Diagnostics:", |ui| {
        ui.horizontal_wrapped(|ui| {
            let running = app.audio_engine.is_running();
            let mut tone = app.test_tone_enabled;
            if ui
                .add_enabled(
                    running,
                    egui::Button::new(if tone {
                        "Test Tone: ON"
                    } else {
                        "Test Tone (440 Hz)"
                    }),
                )
                .on_hover_text("Inject -18 dBFS 440 Hz sine through all FX racks")
                .clicked()
            {
                tone = !tone;
                app.set_test_tone(tone);
            }
            let mut tuner = app.tuner_enabled;
            if ui
                .add_enabled(running, egui::Checkbox::new(&mut tuner, "Chromatic Tuner"))
                .changed()
            {
                app.set_tuner_enabled(tuner);
            }
        });
    });
}

fn draw_settings_plugins_page(app: &mut StageHostApp, ui: &mut egui::Ui) {
    draw_settings_page_banner(ui, "VST / CLAP Plugins");

    ui.label(RichText::new("Plugin Folders:").color(TEXT_PRIMARY));
    ui.add_space(4.0);

    let list_height = 200.0;
    let total_folders = app.plugin_folders.len();
    let mut toggled_enable = false;

    egui::Frame::new()
        .fill(BG_WELL)
        .stroke(Stroke::new(1.0, BORDER_SUBTLE))
        .corner_radius(CornerRadius::same(2))
        .inner_margin(4.0)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.set_min_height(list_height);
            egui::ScrollArea::vertical()
                .id_salt("plugin_folders_scroll")
                .max_height(list_height)
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    if app.plugin_folders.is_empty() {
                        ui.add_space(12.0);
                        ui.add(
                            egui::Label::new(
                                RichText::new("No plugin folders configured. Click Add -> Browse Folder or Restore Default Folders.")
                                    .small()
                                    .color(TEXT_MUTED),
                            )
                            .wrap(),
                        );
                    }
                    for idx in 0..total_folders {
                        let is_selected = app.selected_plugin_folder_idx == Some(idx);
                        let row_bg = if is_selected {
                            BG_RACK_HEADER
                        } else {
                            zebra_row_bg(idx)
                        };
                        let entry = &mut app.plugin_folders[idx];
                        let exists = entry.path.is_dir();
                        let path_text = entry.path.display().to_string();
                        let display_text = if exists {
                            path_text.clone()
                        } else {
                            format!("{path_text} (not found)")
                        };
                        let hover_text = if exists {
                            path_text
                        } else {
                            format!("{path_text} (folder does not exist on disk)")
                        };

                        egui::Frame::new()
                            .fill(row_bg)
                            .inner_margin(egui::Margin::symmetric(4, 2))
                            .show(ui, |ui| {
                                ui.set_width(ui.available_width());
                                ui.horizontal(|ui| {
                                    if ui.checkbox(&mut entry.enabled, "").changed() {
                                        app.selected_plugin_folder_idx = Some(idx);
                                        toggled_enable = true;
                                    }
                                    let text_col = if !entry.enabled {
                                        TEXT_MUTED
                                    } else if is_selected {
                                        ACCENT_AMBER
                                    } else {
                                        TEXT_PRIMARY
                                    };
                                    let resp = ui
                                        .add(
                                            egui::Label::new(
                                                RichText::new(&display_text).color(text_col),
                                            )
                                            .truncate()
                                            .sense(egui::Sense::click()),
                                        )
                                        .on_hover_text(&hover_text);
                                    if resp.clicked() {
                                        app.selected_plugin_folder_idx = Some(idx);
                                    }
                                });
                            });
                    }
                });
        });

    if toggled_enable {
        app.sync_enabled_plugin_search_paths();
        app.save_preferences();
    }

    ui.add_space(6.0);
    let sel_idx = app.selected_plugin_folder_idx;
    let can_remove = sel_idx.is_some_and(|idx| idx < app.plugin_folders.len());
    let can_move_up = sel_idx.is_some_and(|idx| idx > 0 && idx < app.plugin_folders.len());
    let can_move_down = sel_idx.is_some_and(|idx| idx + 1 < app.plugin_folders.len());

    let row_w = ui.available_width();
    let actions_w = 228.0;
    if row_w >= 460.0 {
        ui.horizontal(|ui| {
            ui.allocate_ui_with_layout(
                Vec2::new((row_w - actions_w - 8.0).max(160.0), 24.0),
                egui::Layout::left_to_right(egui::Align::Center),
                |ui| {
                    if ui
                        .checkbox(
                            &mut app.rescan_plugins_on_startup,
                            "Rescan plugin folders on startup",
                        )
                        .changed()
                    {
                        app.save_preferences();
                    }
                },
            );
            ui.allocate_ui_with_layout(
                Vec2::new(actions_w, 24.0),
                egui::Layout::right_to_left(egui::Align::Center),
                |ui| {
                    draw_plugin_folder_action_buttons(app, ui, can_remove, can_move_up, can_move_down);
                },
            );
        });
    } else {
        if ui
            .checkbox(
                &mut app.rescan_plugins_on_startup,
                "Rescan plugin folders on startup",
            )
            .changed()
        {
            app.save_preferences();
        }
        ui.add_space(4.0);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            draw_plugin_folder_action_buttons(app, ui, can_remove, can_move_up, can_move_down);
        });
    }

    if app.show_custom_plugin_path_input {
        ui.add_space(6.0);
        egui::Frame::new()
            .fill(BG_PANEL)
            .stroke(Stroke::new(1.0, BORDER_HAIRLINE))
            .corner_radius(CornerRadius::same(2))
            .inner_margin(6.0)
            .show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new("Custom Path:").small().color(TEXT_SECONDARY));
                    let enter_pressed = ui
                        .add(
                            egui::TextEdit::singleline(&mut app.new_plugin_dir_input)
                                .hint_text("C:\\Program Files\\Common Files\\VST3")
                                .desired_width(280.0),
                        )
                        .lost_focus()
                        && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    if ui.button("Add Folder").clicked() || enter_pressed {
                        let path_str = app.new_plugin_dir_input.trim().to_string();
                        if !path_str.is_empty() {
                            app.add_plugin_folder(PathBuf::from(path_str));
                            app.new_plugin_dir_input.clear();
                            app.show_custom_plugin_path_input = false;
                        }
                    }
                    if ui.button("Cancel").clicked() {
                        app.show_custom_plugin_path_input = false;
                    }
                });
            });
    }

    ui.add_space(8.0);
    ui.separator();
    ui.add_space(4.0);

    let scanning = app.is_scanning_plugins();
    let enabled_folders = app.plugin_folders.iter().filter(|e| e.enabled).count();
    let vst3_count = app
        .scanned_plugins
        .iter()
        .filter(|p| p.format == PluginFormat::Vst3)
        .count();
    let vst2_count = app
        .scanned_plugins
        .iter()
        .filter(|p| p.format == PluginFormat::Vst2)
        .count();
    let clap_count = app
        .scanned_plugins
        .iter()
        .filter(|p| p.format == PluginFormat::Clap)
        .count();

    ui.horizontal_wrapped(|ui| {
        if ui
            .add_enabled(
                !scanning && enabled_folders > 0,
                egui::Button::new("Scan Enabled Folders Now"),
            )
            .clicked()
        {
            app.start_plugin_scan();
        }
        if scanning {
            ui.spinner();
            ui.label(
                RichText::new("Scanning in isolated child processes...")
                    .small()
                    .color(TEXT_SECONDARY),
            );
        } else {
            ui.add(
                egui::Label::new(
                    RichText::new(format!(
                        "{} plugins indexed ({vst3_count} VST3, {vst2_count} VST2, {clap_count} CLAP) across {enabled_folders}/{} enabled folders",
                        app.scanned_plugins.len(),
                        app.plugin_folders.len(),
                    ))
                    .small()
                    .color(TEXT_SECONDARY),
                )
                .wrap(),
            );
        }
    });
    ui.add_space(4.0);
    ui.add(
        egui::Label::new(
            RichText::new("Scans .vst3, 64-bit VST2 .dll, and .clap plugins out-of-process with per-plugin timeout protection (32-bit x86 DLLs are skipped).")
                .small()
                .color(TEXT_MUTED),
        )
        .wrap(),
    );
    if !scanning && !app.last_plugin_scan_issues.is_empty() {
        ui.add_space(6.0);
        egui::CollapsingHeader::new(
            RichText::new(format!(
                "Skipped / Failed Plugins ({})",
                app.last_plugin_scan_issues.len()
            ))
            .small()
            .color(ACCENT_AMBER),
        )
        .default_open(false)
        .show(ui, |ui| {
            egui::ScrollArea::vertical()
                .id_salt("plugin_scan_issues_scroll")
                .max_height(150.0)
                .show(ui, |ui| {
                    for issue in &app.last_plugin_scan_issues {
                        let file_label = issue
                            .path
                            .file_name()
                            .and_then(|s| s.to_str())
                            .map(str::to_string)
                            .unwrap_or_else(|| issue.path.display().to_string());
                        ui.horizontal_wrapped(|ui| {
                            let tag = if issue.timed_out { "[TIMEOUT]" } else { "[SKIP]" };
                            ui.label(RichText::new(tag).small().strong().color(ACCENT_AMBER));
                            ui.label(RichText::new(&file_label).small().strong())
                                .on_hover_text(issue.path.display().to_string());
                            ui.label(
                                RichText::new(format!("- {}", issue.reason))
                                    .small()
                                    .color(TEXT_MUTED),
                            );
                        });
                    }
                });
        });
    }
}

fn draw_plugin_folder_action_buttons(
    app: &mut StageHostApp,
    ui: &mut egui::Ui,
    can_remove: bool,
    can_move_up: bool,
    can_move_down: bool,
) {
    if ui
        .add_enabled(
            can_move_down,
            egui::Button::new("▼").min_size(Vec2::new(28.0, 22.0)),
        )
        .on_hover_text("Move selected folder down")
        .clicked()
    {
        app.move_selected_plugin_folder(1);
    }
    if ui
        .add_enabled(
            can_move_up,
            egui::Button::new("▲").min_size(Vec2::new(28.0, 22.0)),
        )
        .on_hover_text("Move selected folder up")
        .clicked()
    {
        app.move_selected_plugin_folder(-1);
    }
    if ui
        .add_enabled(
            can_remove,
            egui::Button::new("Remove").min_size(Vec2::new(72.0, 22.0)),
        )
        .clicked()
    {
        app.remove_selected_plugin_folder();
    }
    ui.menu_button("Add  v", |ui| {
        if ui.button("Browse Folder...").clicked() {
            if let Some(dir) = file_dialog::pick_folder("Select VST3 / CLAP Plugin Folder") {
                app.add_plugin_folder(dir);
            }
            ui.close();
        }
        if ui.button("Enter Custom Path...").clicked() {
            app.show_custom_plugin_path_input = true;
            ui.close();
        }
        ui.separator();
        if ui.button("Restore Default Folders").clicked() {
            app.restore_default_plugin_folders();
            ui.close();
        }
    });
}

fn draw_settings_general_page(app: &mut StageHostApp, ui: &mut egui::Ui) {
    draw_settings_page_banner(ui, "General & Session Behavior");

    if ui
        .checkbox(
            &mut app.auto_save_on_song_switch,
            "Auto-save current session before switching Set List songs",
        )
        .changed()
    {
        app.save_preferences();
    }
    ui.add_space(6.0);
    if ui
        .checkbox(
            &mut app.reopen_last_session,
            "Reopen last saved session and set list on application startup",
        )
        .changed()
    {
        app.save_preferences();
    }
    ui.add_space(6.0);
    draw_setting_row(ui, "Auto-load Config:", |ui| {
        let selected_label = app
            .startup_config_path
            .as_ref()
            .map(|p| {
                let name = p
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or("Unknown File");
                if let Some(entry) = app
                    .set_list
                    .entries
                    .iter()
                    .find(|e| &e.session_path == p)
                {
                    format!("{} ({name})", entry.title)
                } else {
                    name.to_string()
                }
            })
            .unwrap_or_else(|| "<None (Use Reopen Last Session / Default)>".to_string());

        let mut next_path: Option<Option<PathBuf>> = None;
        ui.horizontal_wrapped(|ui| {
            egui::ComboBox::from_id_salt("startup_config_combo")
                .width(ui.available_width().clamp(180.0, 320.0))
                .selected_text(selected_label)
                .show_ui(ui, |ui| {
                    if ui
                        .selectable_label(
                            app.startup_config_path.is_none(),
                            "<None (Use Reopen Last Session / Default)>",
                        )
                        .clicked()
                    {
                        next_path = Some(None);
                    }
                    for entry in &app.set_list.entries {
                        let is_sel = app
                            .startup_config_path
                            .as_ref()
                            .is_some_and(|p| p == &entry.session_path);
                        let fname = entry
                            .session_path
                            .file_name()
                            .and_then(|s| s.to_str())
                            .unwrap_or("");
                        let row_txt = if fname.is_empty() {
                            entry.title.clone()
                        } else {
                            format!("{} ({fname})", entry.title)
                        };
                        if ui
                            .selectable_label(is_sel, row_txt)
                            .on_hover_text(entry.session_path.display().to_string())
                            .clicked()
                        {
                            next_path = Some(Some(entry.session_path.clone()));
                        }
                    }
                });
            if ui
                .button("Browse...")
                .on_hover_text("Pick a .stagehost-settings.json or .stagehost.json file to auto-load on startup")
                .clicked()
            {
                if let Some(path) = file_dialog::pick_open_file(
                    "Select Startup Session / Config File",
                    "StageHost Config (*.stagehost-settings.json, *.stagehost.json)",
                    "*.stagehost-settings.json;*.stagehost.json;*.json",
                ) {
                    next_path = Some(Some(path));
                }
            }
            if app.startup_config_path.is_some()
                && ui
                    .button("Clear")
                    .on_hover_text("Disable fixed startup config file")
                    .clicked()
            {
                next_path = Some(None);
            }
        });
        if let Some(chosen) = next_path {
            app.set_startup_config_path(chosen);
        }
        if let Some(ref p) = app.startup_config_path {
            let exists = p.is_file();
            let hint_color = if exists { TEXT_MUTED } else { AUDIO_RED };
            let hint_text = if exists {
                p.display().to_string()
            } else {
                format!("Missing file: {}", p.display())
            };
            ui.add(egui::Label::new(RichText::new(hint_text).small().color(hint_color)).wrap());
        }
    });
    ui.add_space(6.0);
    if ui
        .checkbox(
            &mut app.start_output_muted,
            "Start application with speaker monitor output muted (input meters stay active)",
        )
        .changed()
    {
        app.save_preferences();
    }
    ui.add_space(6.0);
    if ui
        .checkbox(
            &mut app.close_to_tray,
            "Minimize to System Tray when closing window (keep audio & global keybinds running)",
        )
        .changed()
    {
        app.save_preferences();
    }
    ui.add_space(6.0);
    let mut auto_run = app.auto_run_on_startup;
    if ui
        .checkbox(
            &mut auto_run,
            "Run StageHost automatically when Windows starts (current user)",
        )
        .on_hover_text("Registers or removes StageHost in HKCU\\Software\\Microsoft\\Windows\\CurrentVersion\\Run")
        .changed()
    {
        app.set_auto_run_on_startup(auto_run);
    }
    ui.add_space(6.0);
    if ui
        .checkbox(
            &mut app.start_in_tray_on_autostart,
            "Start directly in System Tray when auto-started with Windows",
        )
        .on_hover_text("When launched at Windows startup (--autostart), load audio & session in the system tray without showing the main window")
        .changed()
    {
        app.save_preferences();
    }
    if app.auto_run_on_startup && !app.is_auto_run_path_current() {
        ui.add_space(4.0);
        ui.horizontal_wrapped(|ui| {
            ui.label(
                RichText::new("Registered startup path points to another executable location.")
                    .small()
                    .color(ACCENT_AMBER),
            );
            if ui.small_button("Update Startup Path").clicked() {
                app.set_auto_run_on_startup(true);
            }
        });
    }

    ui.add_space(14.0);
    draw_settings_page_banner(ui, "Configuration & Active Paths");

    let config_dir = default_app_config_dir();
    let session_str = app
        .last_saved_session_path
        .as_ref()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "<Unsaved session>".to_string());
    let setlist_str = app
        .last_saved_setlist_path
        .as_ref()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "<Unsaved set list>".to_string());

    draw_setting_row(ui, "App Config Folder:", |ui| {
        ui.add(
            egui::Label::new(
                RichText::new(config_dir.display().to_string())
                    .small()
                    .color(TEXT_PRIMARY),
            )
            .wrap(),
        );
    });
    draw_setting_row(ui, "Active Session File:", |ui| {
        ui.add(egui::Label::new(RichText::new(session_str).small().color(TEXT_PRIMARY)).wrap());
    });
    draw_setting_row(ui, "Active Set List File:", |ui| {
        ui.add(egui::Label::new(RichText::new(setlist_str).small().color(TEXT_PRIMARY)).wrap());
    });
    draw_setting_row(ui, "Windows Startup:", |ui| {
        let (startup_text, startup_color) = match &app.auto_run_registered_cmd {
            Some(cmd) => (cmd.as_str(), TEXT_PRIMARY),
            None => ("Disabled", TEXT_MUTED),
        };
        ui.add(egui::Label::new(RichText::new(startup_text).small().color(startup_color)).wrap());
    });

    ui.add_space(12.0);
    draw_settings_page_banner(ui, "Backup & Portable Settings File");
    ui.label(
        RichText::new("Save all application settings (Audio/ASIO, plugin folders, scanned plugins, session racks, strip colors, routing cables, and set list) to a single file.")
            .small()
            .color(TEXT_SECONDARY),
    );
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        if ui.button("Save All Settings to File...").clicked() {
            if let Some(path) = file_dialog::pick_save_file(
                "Save All StageHost Settings",
                "stagehost.stagehost-settings.json",
                "StageHost All Settings (*.stagehost-settings.json)",
                "*.stagehost-settings.json",
            ) {
                app.export_all_settings_file(&path);
            }
        }
        if ui.button("Load All Settings from File...").clicked() {
            if let Some(path) = file_dialog::pick_open_file(
                "Load All StageHost Settings",
                "StageHost All Settings (*.stagehost-settings.json)",
                "*.stagehost-settings.json;*.json",
            ) {
                app.import_all_settings_file(&path);
            }
        }
        if ui.button("Save Config Now").clicked() {
            app.capture_live_plugin_states();
            app.save_preferences();
            app.save_autosave_workspace();
            app.set_status("Saved preferences and workspace autosave to AppData", false);
        }
    });
}

fn draw_settings_workspace_page(app: &mut StageHostApp, ui: &mut egui::Ui) {
    draw_settings_page_banner(ui, "Workspace Layout");

    if ui
        .checkbox(
            &mut app.show_left_pane,
            "Show Left Pane (Set List, Sub-Sessions & Files)",
        )
        .changed()
    {
        app.save_preferences();
    }
    ui.add_space(6.0);
    if ui
        .checkbox(&mut app.show_io_strip, "Show Master & Input I/O Meter Strip")
        .changed()
    {
        app.save_preferences();
    }

    ui.add_space(14.0);
    draw_settings_page_banner(ui, "Plugin & Global Key Bindings");

    ui.add(
        egui::Label::new(
            RichText::new(format!(
                "{} custom plugin/rack key binding(s) configured in current session",
                app.session.key_bindings.len()
            ))
            .color(TEXT_SECONDARY),
        )
        .wrap(),
    );
    ui.add_space(6.0);
    if ui.button("Open Key Bindings Manager...").clicked() {
        open_keybindings_window(app, ui.ctx(), None);
    }

    ui.add_space(12.0);
    ui.label(RichText::new("Built-In Global Shortcuts (reserved):").color(TEXT_PRIMARY));
    ui.add_space(4.0);
    egui::Frame::new()
        .fill(BG_WELL)
        .stroke(Stroke::new(1.0, BORDER_SUBTLE))
        .corner_radius(CornerRadius::same(2))
        .inner_margin(8.0)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            for (key, desc) in [
                ("Space / PageDown / Down", "Next Sub-Session or Song"),
                ("PageUp / Up", "Previous Sub-Session or Song"),
                ("Alt + 1..9", "Jump directly to Sub-Session 1..9"),
                ("Escape", "Audio Panic (Reset DSP & Plugin Buffers)"),
                ("Ctrl + S", "Save Current Session"),
                ("Ctrl + Z / Ctrl + Y", "Undo / Redo Session Edit"),
            ] {
                draw_setting_row_custom(ui, 176.0, key, ACCENT_AMBER, true, |ui| {
                    ui.add(egui::Label::new(RichText::new(desc).color(TEXT_SECONDARY)).wrap());
                });
            }
        });
}

#[derive(Clone)]
pub(crate) struct PluginBrowserItem {
    key: String,
    name: String,
    vendor: String,
    version: String,
    format_label: &'static str,
    format: PluginFormat,
    is_instrument: bool,
    audio_inputs: usize,
    audio_outputs: usize,
    has_sidechain: bool,
    detail: String,
    kind: PluginKind,
}

fn ensure_browser_catalog_cached(app: &mut StageHostApp) {
    if app.browser_catalog_dirty {
        app.browser_catalog_cache = collect_browser_catalog(app);
        app.browser_vendors_cache = collect_vendor_counts(&app.browser_catalog_cache);
        app.browser_catalog_dirty = false;
    }
}

fn collect_browser_catalog(app: &StageHostApp) -> Vec<PluginBrowserItem> {
    let mut items = Vec::new();
    for (idx, (name, desc, kind)) in StageHostApp::builtin_catalog().into_iter().enumerate() {
        let has_sidechain = matches!(
            kind,
            PluginKind::BuiltInEffect(BuiltInEffectKind::DynamicsGateComp)
        );
        items.push(PluginBrowserItem {
            key: format!("BUILTIN:{idx}:{name}"),
            name: name.to_string(),
            vendor: "StageHost Built-In".to_string(),
            version: "1.0".to_string(),
            format_label: "DSP",
            format: PluginFormat::BuiltIn,
            is_instrument: false,
            audio_inputs: 2,
            audio_outputs: 2,
            has_sidechain,
            detail: desc.to_string(),
            kind,
        });
    }

    let mut externals: Vec<PluginBrowserItem> = app
        .scanned_plugins
        .iter()
        .filter_map(|desc| {
            let kind = desc.to_plugin_kind()?;
            let format_label = match desc.format {
                PluginFormat::Vst3 => "VST3",
                PluginFormat::Vst2 => "VST2",
                PluginFormat::Clap => "CLAP",
                PluginFormat::BuiltIn => "DSP",
            };
            let vendor = if desc.vendor.trim().is_empty() {
                "Unknown Vendor".to_string()
            } else {
                desc.vendor.clone()
            };
            let detail = desc
                .path
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| desc.id.clone());
            Some(PluginBrowserItem {
                key: format!("{format_label}:{}", desc.id),
                name: desc.name.clone(),
                vendor,
                version: desc.version.clone(),
                format_label,
                format: desc.format,
                is_instrument: desc.is_instrument,
                audio_inputs: desc.audio_inputs,
                audio_outputs: desc.audio_outputs,
                has_sidechain: desc.has_sidechain,
                detail,
                kind,
            })
        })
        .collect();
    externals.sort_by(|a, b| {
        a.name
            .to_ascii_lowercase()
            .cmp(&b.name.to_ascii_lowercase())
            .then_with(|| a.format_label.cmp(b.format_label))
    });
    items.extend(externals);
    items
}

fn collect_vendor_counts(catalog: &[PluginBrowserItem]) -> Vec<(String, usize)> {
    let mut counts: HashMap<String, usize> = HashMap::new();
    for item in catalog {
        *counts.entry(item.vendor.clone()).or_insert(0) += 1;
    }
    let mut list: Vec<(String, usize)> = counts.into_iter().collect();
    list.sort_by_key(|a| a.0.to_ascii_lowercase());
    list
}

fn matches_browser_filters(app: &StageHostApp, item: &PluginBrowserItem, query: &str) -> bool {
    let format_ok = match app.plugin_filter_format {
        PluginFilterFormat::All => true,
        PluginFilterFormat::BuiltIn => item.format == PluginFormat::BuiltIn,
        PluginFilterFormat::Vst3 => item.format == PluginFormat::Vst3,
        PluginFilterFormat::Vst2 => item.format == PluginFormat::Vst2,
        PluginFilterFormat::Clap => item.format == PluginFormat::Clap,
    };
    if !format_ok {
        return false;
    }
    let role_ok = match app.plugin_filter_role {
        PluginFilterRole::All => true,
        PluginFilterRole::Effects => !item.is_instrument,
        PluginFilterRole::Instruments => item.is_instrument,
    };
    if !role_ok {
        return false;
    }
    if let Some(ref vendor) = app.plugin_filter_vendor {
        if !item.vendor.eq_ignore_ascii_case(vendor) {
            return false;
        }
    }
    if !query.is_empty() {
        let hit = item.name.to_ascii_lowercase().contains(query)
            || item.vendor.to_ascii_lowercase().contains(query)
            || item.format_label.to_ascii_lowercase().contains(query)
            || item.detail.to_ascii_lowercase().contains(query);
        if !hit {
            return false;
        }
    }
    true
}

fn handle_plugin_browser_keyboard(
    app: &mut StageHostApp,
    ctx: &egui::Context,
    filtered: &[&PluginBrowserItem],
) -> Option<(String, PluginKind)> {
    if filtered.is_empty() {
        return None;
    }
    let cur_idx = app
        .selected_browser_plugin_key
        .as_deref()
        .and_then(|key| filtered.iter().position(|item| item.key == key))
        .unwrap_or(0);

    let (move_up, move_down, confirm) = ctx.input(|i| {
        (
            i.key_pressed(egui::Key::ArrowUp),
            i.key_pressed(egui::Key::ArrowDown),
            i.key_pressed(egui::Key::Enter),
        )
    });

    if move_up {
        let next = cur_idx.saturating_sub(1);
        app.selected_browser_plugin_key = Some(filtered[next].key.clone());
    } else if move_down {
        let next = (cur_idx + 1).min(filtered.len() - 1);
        app.selected_browser_plugin_key = Some(filtered[next].key.clone());
    } else if confirm {
        let item = filtered[cur_idx];
        return Some((item.name.clone(), item.kind.clone()));
    }
    None
}

fn draw_add_plugin_header(app: &mut StageHostApp, ui: &mut egui::Ui) {
    ui.horizontal_wrapped(|ui| {
        ui.label(RichText::new("Search:").color(TEXT_SECONDARY));
        let edit_resp = ui.add(
            egui::TextEdit::singleline(&mut app.plugin_filter_query)
                .id_salt("insert_plugin_search_box")
                .hint_text("Plugin name, vendor, or format...")
                .desired_width(230.0),
        );
        if app.focus_plugin_search_box {
            edit_resp.request_focus();
            app.focus_plugin_search_box = false;
        }
        if !app.plugin_filter_query.is_empty()
            && ui
                .small_button("x")
                .on_hover_text("Clear search filter")
                .clicked()
        {
            app.plugin_filter_query.clear();
        }

        ui.separator();
        for (fmt, label) in [
            (PluginFilterFormat::All, "All"),
            (PluginFilterFormat::BuiltIn, "Built-In"),
            (PluginFilterFormat::Vst3, "VST3"),
            (PluginFilterFormat::Vst2, "VST2"),
            (PluginFilterFormat::Clap, "CLAP"),
        ] {
            ui.selectable_value(&mut app.plugin_filter_format, fmt, label);
        }

        ui.separator();
        for (role, label) in [
            (PluginFilterRole::All, "All Types"),
            (PluginFilterRole::Effects, "Effects"),
            (PluginFilterRole::Instruments, "Instruments"),
        ] {
            ui.selectable_value(&mut app.plugin_filter_role, role, label);
        }

        ui.separator();
        let scanning = app.is_scanning_plugins();
        if scanning {
            ui.spinner();
            ui.label(RichText::new("Scanning...").small().color(ACCENT_AMBER));
        } else if ui
            .button("Scan")
            .on_hover_text("Rescan enabled VST3, VST2, and CLAP folders")
            .clicked()
        {
            app.start_plugin_scan();
        }
        if ui
            .button("Folders...")
            .on_hover_text("Configure plugin search folders in Options")
            .clicked()
        {
            open_options_window(app, ui.ctx(), Some(SettingsCategory::Plugins));
        }
    });
}

fn draw_add_plugin_sidebar(
    app: &mut StageHostApp,
    ui: &mut egui::Ui,
    catalog: &[PluginBrowserItem],
    vendors: &[(String, usize)],
) {
    let total_count = catalog.len();
    let builtin_count = catalog
        .iter()
        .filter(|item| item.format == PluginFormat::BuiltIn)
        .count();
    let effect_count = catalog.iter().filter(|item| !item.is_instrument).count();
    let inst_count = catalog.iter().filter(|item| item.is_instrument).count();

    ui.label(RichText::new("CATEGORY").small().strong().color(TEXT_MUTED));
    ui.add_space(4.0);

    let is_all = app.plugin_filter_format == PluginFilterFormat::All
        && app.plugin_filter_role == PluginFilterRole::All
        && app.plugin_filter_vendor.is_none();
    if draw_settings_nav_item(ui, &format!("All Plugins ({total_count})"), is_all) {
        app.plugin_filter_format = PluginFilterFormat::All;
        app.plugin_filter_role = PluginFilterRole::All;
        app.plugin_filter_vendor = None;
    }
    let is_builtin = app.plugin_filter_format == PluginFilterFormat::BuiltIn
        && app.plugin_filter_vendor.is_none();
    if draw_settings_nav_item(ui, &format!("Built-In DSP ({builtin_count})"), is_builtin) {
        app.plugin_filter_format = PluginFilterFormat::BuiltIn;
        app.plugin_filter_role = PluginFilterRole::All;
        app.plugin_filter_vendor = None;
    }
    let is_effects = app.plugin_filter_format == PluginFilterFormat::All
        && app.plugin_filter_role == PluginFilterRole::Effects
        && app.plugin_filter_vendor.is_none();
    if draw_settings_nav_item(ui, &format!("Effects ({effect_count})"), is_effects) {
        app.plugin_filter_format = PluginFilterFormat::All;
        app.plugin_filter_role = PluginFilterRole::Effects;
        app.plugin_filter_vendor = None;
    }
    let is_insts = app.plugin_filter_format == PluginFilterFormat::All
        && app.plugin_filter_role == PluginFilterRole::Instruments
        && app.plugin_filter_vendor.is_none();
    if draw_settings_nav_item(ui, &format!("Instruments ({inst_count})"), is_insts) {
        app.plugin_filter_format = PluginFilterFormat::All;
        app.plugin_filter_role = PluginFilterRole::Instruments;
        app.plugin_filter_vendor = None;
    }

    ui.add_space(8.0);
    ui.separator();
    ui.add_space(6.0);
    ui.label(RichText::new("BY VENDOR").small().strong().color(TEXT_MUTED));
    ui.add_space(4.0);

    egui::ScrollArea::vertical()
        .id_salt("add_plugin_vendor_scroll")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            let all_vendors_sel = app.plugin_filter_vendor.is_none();
            if draw_settings_nav_item(ui, "All Vendors", all_vendors_sel) {
                app.plugin_filter_vendor = None;
            }
            for (vendor, count) in vendors {
                let selected = app
                    .plugin_filter_vendor
                    .as_deref()
                    .is_some_and(|v| v.eq_ignore_ascii_case(vendor));
                if draw_settings_nav_item(ui, &format!("{vendor} ({count})"), selected) {
                    app.plugin_filter_vendor = Some(vendor.clone());
                }
            }
        });
}

fn draw_add_plugin_table(
    app: &mut StageHostApp,
    ui: &mut egui::Ui,
    filtered: &[&PluginBrowserItem],
) -> Option<(String, PluginKind)> {
    let mut double_clicked_choice: Option<(String, PluginKind)> = None;
    let header_w = ui.available_width().max(380.0);
    let col_fmt_w = 56.0;
    let col_type_w = 84.0;
    let col_io_w = 78.0;
    let fixed_w = col_fmt_w + col_type_w + col_io_w + 32.0;
    let flex_w = (header_w - fixed_w).max(160.0);
    let col_name_w = (flex_w * 0.58).floor();
    let col_vendor_w = (flex_w - col_name_w).max(80.0);

    egui::Frame::new()
        .fill(BG_RACK_HEADER)
        .stroke(Stroke::new(1.0, BORDER_SUBTLE))
        .corner_radius(CornerRadius::same(2))
        .inner_margin(egui::Margin::symmetric(8, 5))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                for (col_w, title) in [
                    (col_fmt_w, "Format"),
                    (col_name_w, "Plugin Name"),
                    (col_vendor_w, "Vendor"),
                    (col_type_w, "Type"),
                    (col_io_w, "I/O"),
                ] {
                    ui.allocate_ui_with_layout(
                        Vec2::new(col_w, 16.0),
                        egui::Layout::left_to_right(egui::Align::Center),
                        |ui| {
                            ui.add(
                                egui::Label::new(
                                    RichText::new(title).small().strong().color(TEXT_SECONDARY),
                                )
                                .truncate(),
                            );
                        },
                    );
                }
            });
        });

    ui.add_space(4.0);

    egui::Frame::new()
        .fill(BG_WELL)
        .stroke(Stroke::new(1.0, BORDER_SUBTLE))
        .corner_radius(CornerRadius::same(2))
        .inner_margin(4.0)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.set_min_height(ui.available_height());
            egui::ScrollArea::vertical()
                .id_salt("add_plugin_table_scroll")
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    if filtered.is_empty() {
                        ui.add_space(16.0);
                        ui.add(
                            egui::Label::new(
                                RichText::new("No plugins match the active filters. Clear the search box or click Scan / Folders... to index external VST3/CLAP plugins.")
                                    .color(TEXT_MUTED),
                            )
                            .wrap(),
                        );
                        return;
                    }

                    for (idx, item) in filtered.iter().enumerate() {
                        let is_selected = app
                            .selected_browser_plugin_key
                            .as_deref()
                            .is_some_and(|k| k == item.key);
                        let row_bg = if is_selected {
                            BG_RACK_HEADER
                        } else {
                            zebra_row_bg(idx)
                        };
                        let row_stroke = if is_selected {
                            Stroke::new(1.0, ACCENT_AMBER)
                        } else {
                            Stroke::NONE
                        };

                        let inner_resp = egui::Frame::new()
                            .fill(row_bg)
                            .stroke(row_stroke)
                            .corner_radius(CornerRadius::same(2))
                            .inner_margin(egui::Margin::symmetric(6, 4))
                            .show(ui, |ui| {
                                ui.set_width(ui.available_width());
                                ui.horizontal(|ui| {
                                    let badge_text_col = match item.format {
                                        PluginFormat::Vst3 | PluginFormat::Vst2 => ACCENT_AMBER,
                                        PluginFormat::Clap => TEXT_PRIMARY,
                                        PluginFormat::BuiltIn => TEXT_SECONDARY,
                                    };
                                    ui.allocate_ui_with_layout(
                                        Vec2::new(col_fmt_w, 18.0),
                                        egui::Layout::left_to_right(egui::Align::Center),
                                        |ui| {
                                            egui::Frame::new()
                                                .fill(BG_PANEL)
                                                .stroke(Stroke::new(1.0, BORDER_HAIRLINE))
                                                .corner_radius(CornerRadius::same(2))
                                                .inner_margin(egui::Margin::symmetric(5, 1))
                                                .show(ui, |ui| {
                                                    ui.label(
                                                        RichText::new(item.format_label)
                                                            .small()
                                                            .strong()
                                                            .monospace()
                                                            .color(badge_text_col),
                                                    );
                                                });
                                        },
                                    );

                                    let name_col = if is_selected {
                                        ACCENT_AMBER
                                    } else {
                                        TEXT_PRIMARY
                                    };
                                    ui.allocate_ui_with_layout(
                                        Vec2::new(col_name_w, 18.0),
                                        egui::Layout::left_to_right(egui::Align::Center),
                                        |ui| {
                                            let r = ui.add(
                                                egui::Label::new(
                                                    RichText::new(&item.name)
                                                        .strong()
                                                        .color(name_col),
                                                )
                                                .truncate()
                                                .selectable(false)
                                                .sense(egui::Sense::click()),
                                            );
                                            if r.double_clicked() {
                                                double_clicked_choice =
                                                    Some((item.name.clone(), item.kind.clone()));
                                            } else if r.clicked() {
                                                app.selected_browser_plugin_key =
                                                    Some(item.key.clone());
                                            }
                                        },
                                    );

                                    ui.allocate_ui_with_layout(
                                        Vec2::new(col_vendor_w, 18.0),
                                        egui::Layout::left_to_right(egui::Align::Center),
                                        |ui| {
                                            let r = ui.add(
                                                egui::Label::new(
                                                    RichText::new(&item.vendor)
                                                        .small()
                                                        .color(TEXT_SECONDARY),
                                                )
                                                .truncate()
                                                .selectable(false)
                                                .sense(egui::Sense::click()),
                                            );
                                            if r.double_clicked() {
                                                double_clicked_choice =
                                                    Some((item.name.clone(), item.kind.clone()));
                                            } else if r.clicked() {
                                                app.selected_browser_plugin_key =
                                                    Some(item.key.clone());
                                            }
                                        },
                                    );

                                    let type_label = if item.is_instrument {
                                        "Instrument"
                                    } else {
                                        "Effect"
                                    };
                                    ui.allocate_ui_with_layout(
                                        Vec2::new(col_type_w, 18.0),
                                        egui::Layout::left_to_right(egui::Align::Center),
                                        |ui| {
                                            ui.add(
                                                egui::Label::new(
                                                    RichText::new(type_label)
                                                        .small()
                                                        .color(TEXT_MUTED),
                                                )
                                                .truncate()
                                                .selectable(false),
                                            );
                                        },
                                    );

                                    let io_label = if item.has_sidechain {
                                        format!("{} -> {} SC", item.audio_inputs, item.audio_outputs)
                                    } else {
                                        format!("{} -> {}", item.audio_inputs, item.audio_outputs)
                                    };
                                    ui.allocate_ui_with_layout(
                                        Vec2::new(col_io_w, 18.0),
                                        egui::Layout::left_to_right(egui::Align::Center),
                                        |ui| {
                                            ui.add(
                                                egui::Label::new(
                                                    RichText::new(io_label)
                                                        .small()
                                                        .monospace()
                                                        .color(TEXT_MUTED),
                                                )
                                                .truncate()
                                                .selectable(false),
                                            );
                                        },
                                    );
                                });
                            });

                        let row_resp = inner_resp
                            .response
                            .interact(egui::Sense::click())
                            .on_hover_text(&item.detail);
                        if row_resp.double_clicked() {
                            double_clicked_choice = Some((item.name.clone(), item.kind.clone()));
                        } else if row_resp.clicked() {
                            app.selected_browser_plugin_key = Some(item.key.clone());
                        }
                    }
                });
        });

    double_clicked_choice
}

fn draw_add_plugin_footer(
    ui: &mut egui::Ui,
    selected: Option<&PluginBrowserItem>,
    shown_count: usize,
    total_count: usize,
    target_rack_name: &str,
) -> (bool, bool) {
    let mut insert_clicked = false;
    let mut cancel_clicked = false;

    if let Some(item) = selected {
        let ver_part = if item.version.trim().is_empty() {
            String::new()
        } else {
            format!(" v{}", item.version)
        };
        let role_part = if item.is_instrument {
            "Instrument"
        } else {
            "Effect"
        };
        ui.add(
            egui::Label::new(
                RichText::new(format!(
                    "[{}] {}{} | {} | {} | {} in / {} out",
                    item.format_label,
                    item.name,
                    ver_part,
                    item.vendor,
                    role_part,
                    item.audio_inputs,
                    item.audio_outputs
                ))
                .small()
                .color(TEXT_PRIMARY),
            )
            .truncate(),
        );
        ui.add(
            egui::Label::new(RichText::new(&item.detail).small().color(TEXT_MUTED)).truncate(),
        )
        .on_hover_text(&item.detail);
    } else {
        ui.label(
            RichText::new("Select a plugin row above, or double-click / press Enter to insert.")
                .small()
                .color(TEXT_MUTED),
        );
    }

    ui.add_space(6.0);
    ui.separator();
    ui.add_space(4.0);

    ui.horizontal(|ui| {
        let footer_w = ui.available_width();
        let btns_w = 212.0;
        ui.allocate_ui_with_layout(
            Vec2::new((footer_w - btns_w - 8.0).max(160.0), 26.0),
            egui::Layout::left_to_right(egui::Align::Center),
            |ui| {
                ui.add(
                    egui::Label::new(
                        RichText::new(format!(
                            "Target Rack: {target_rack_name}  |  {shown_count}/{total_count} plugins"
                        ))
                        .small()
                        .color(TEXT_MUTED),
                    )
                    .truncate(),
                );
            },
        );
        ui.allocate_ui_with_layout(
            Vec2::new(btns_w, 26.0),
            egui::Layout::right_to_left(egui::Align::Center),
            |ui| {
                let insert_btn = egui::Button::new(
                    RichText::new("Insert Plugin")
                        .strong()
                        .color(if selected.is_some() {
                            TEXT_ON_ACCENT
                        } else {
                            TEXT_MUTED
                        }),
                )
                .fill(if selected.is_some() {
                    ACCENT_AMBER
                } else {
                    BG_WELL
                })
                .min_size(Vec2::new(112.0, 24.0));
                if ui.add_enabled(selected.is_some(), insert_btn).clicked() {
                    insert_clicked = true;
                }
                if ui
                    .add_sized([84.0, 24.0], egui::Button::new("Cancel"))
                    .clicked()
                {
                    cancel_clicked = true;
                }
            },
        );
    });

    (insert_clicked, cancel_clicked)
}

fn draw_add_plugin_modal(app: &mut StageHostApp, ctx: &egui::Context) {
    let Some(target_rack) = app.add_slot_target_rack_id else {
        return;
    };
    let target_rack_name = app
        .session
        .racks
        .iter()
        .find(|r| r.id == target_rack)
        .map(|r| r.name.clone())
        .unwrap_or_else(|| "Selected Rack".to_string());
    let mut open = true;
    let mut chosen: Option<(String, PluginKind)> = None;

    let builder = egui::ViewportBuilder::default()
        .with_title("Insert Plugin - StageHost")
        .with_inner_size([840.0, 560.0])
        .with_min_inner_size([680.0, 440.0]);

    ctx.show_viewport_immediate(add_plugin_viewport_id(), builder, |ui, _class| {
        if ui.ctx().input(|i| {
            i.viewport().close_requested() || i.key_pressed(egui::Key::Escape)
        }) {
            open = false;
        }

        let mut catalog = Vec::new();
        let mut vendors = Vec::new();
        ensure_browser_catalog_cached(app);
        std::mem::swap(&mut catalog, &mut app.browser_catalog_cache);
        std::mem::swap(&mut vendors, &mut app.browser_vendors_cache);

        egui::Panel::top("add_plugin_filter_bar")
            .frame(
                egui::Frame::new()
                    .fill(BG_PANEL)
                    .stroke(Stroke::new(1.0, BORDER_HAIRLINE))
                    .inner_margin(egui::Margin::symmetric(12, 8)),
            )
            .show(ui, |ui| {
                draw_add_plugin_header(app, ui);
            });

        let query = app.plugin_filter_query.trim().to_ascii_lowercase();
        let filtered: Vec<&PluginBrowserItem> = catalog
            .iter()
            .filter(|item| matches_browser_filters(app, item, &query))
            .collect();

        if let Some(kb_choice) = handle_plugin_browser_keyboard(app, ui.ctx(), &filtered) {
            chosen = Some(kb_choice);
        }

        if !filtered.is_empty()
            && app
                .selected_browser_plugin_key
                .as_deref()
                .is_none_or(|k| !filtered.iter().any(|item| item.key == k))
        {
            app.selected_browser_plugin_key = Some(filtered[0].key.clone());
        }

        let selected_item = app
            .selected_browser_plugin_key
            .as_deref()
            .and_then(|key| filtered.iter().copied().find(|item| item.key == key));

        egui::Panel::bottom("add_plugin_footer")
            .frame(
                egui::Frame::new()
                    .fill(BG_PANEL)
                    .stroke(Stroke::new(1.0, BORDER_HAIRLINE))
                    .inner_margin(egui::Margin::symmetric(12, 8)),
            )
            .show(ui, |ui| {
                let (insert_clicked, cancel_clicked) = draw_add_plugin_footer(
                    ui,
                    selected_item,
                    filtered.len(),
                    catalog.len(),
                    &target_rack_name,
                );
                if insert_clicked {
                    if let Some(item) = selected_item {
                        chosen = Some((item.name.clone(), item.kind.clone()));
                    }
                }
                if cancel_clicked {
                    open = false;
                }
            });

        egui::Panel::left("add_plugin_vendor_sidebar")
            .resizable(false)
            .default_size(188.0)
            .size_range(172.0..=210.0)
            .frame(
                egui::Frame::new()
                    .fill(BG_PANEL)
                    .stroke(Stroke::new(1.0, BORDER_HAIRLINE))
                    .inner_margin(egui::Margin::symmetric(8, 10)),
            )
            .show(ui, |ui| {
                draw_add_plugin_sidebar(app, ui, &catalog, &vendors);
            });

        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(BG_CHASSIS)
                    .inner_margin(egui::Margin::same(10)),
            )
            .show(ui, |ui| {
                if let Some(dbl_choice) = draw_add_plugin_table(app, ui, &filtered) {
                    chosen = Some(dbl_choice);
                }
            });

        app.browser_catalog_cache = catalog;
        app.browser_vendors_cache = vendors;
    });

    if let Some((name, kind)) = chosen {
        app.add_slot_to_rack(target_rack, name, kind);
        open = false;
    }
    if !open {
        app.add_slot_target_rack_id = None;
    }
}

fn describe_key_binding(app: &StageHostApp, action: &KeyBindingAction) -> (String, String) {
    let find_rack = |rid: Uuid| app.session.racks.iter().find(|r| r.id == rid);
    match action {
        KeyBindingAction::ToggleSlotEnabled { rack_id, slot_id } => {
            let target = find_rack(*rack_id)
                .and_then(|r| {
                    r.slots
                        .iter()
                        .find(|s| s.id == *slot_id)
                        .map(|s| format!("{} > {}", r.name, s.name))
                })
                .unwrap_or_else(|| "<Missing Plugin Slot>".to_string());
            (target, "Toggle Enable".to_string())
        }
        KeyBindingAction::ToggleSlotBypass { rack_id, slot_id } => {
            let target = find_rack(*rack_id)
                .and_then(|r| {
                    r.slots
                        .iter()
                        .find(|s| s.id == *slot_id)
                        .map(|s| format!("{} > {}", r.name, s.name))
                })
                .unwrap_or_else(|| "<Missing Plugin Slot>".to_string());
            (target, "Toggle Bypass".to_string())
        }
        KeyBindingAction::OpenSlotEditor { rack_id, slot_id } => {
            let target = find_rack(*rack_id)
                .and_then(|r| {
                    r.slots
                        .iter()
                        .find(|s| s.id == *slot_id)
                        .map(|s| format!("{} > {}", r.name, s.name))
                })
                .unwrap_or_else(|| "<Missing Plugin Slot>".to_string());
            (target, "Open Plugin GUI".to_string())
        }
        KeyBindingAction::SetSlotParam {
            rack_id,
            slot_id,
            param_id,
            value,
        } => {
            let target = find_rack(*rack_id)
                .and_then(|r| {
                    r.slots
                        .iter()
                        .find(|s| s.id == *slot_id)
                        .map(|s| format!("{} > {}", r.name, s.name))
                })
                .unwrap_or_else(|| "<Missing Plugin Slot>".to_string());
            let param_name = app
                .slot_param_catalog
                .get(slot_id)
                .and_then(|list| list.iter().find(|p| p.id == *param_id))
                .map(|p| p.name.clone())
                .unwrap_or_else(|| format!("Param #{param_id}"));
            (target, format!("Set {param_name} = {value:.3}"))
        }
        KeyBindingAction::ToggleRackMute { rack_id } => {
            let target = find_rack(*rack_id)
                .map(|r| r.name.clone())
                .unwrap_or_else(|| "<Missing Rack>".to_string());
            (target, "Toggle Rack Mute".to_string())
        }
        KeyBindingAction::ToggleRackSolo { rack_id } => {
            let target = find_rack(*rack_id)
                .map(|r| r.name.clone())
                .unwrap_or_else(|| "<Missing Rack>".to_string());
            (target, "Toggle Rack Solo".to_string())
        }
        KeyBindingAction::ToggleRackEnabled { rack_id } => {
            let target = find_rack(*rack_id)
                .map(|r| r.name.clone())
                .unwrap_or_else(|| "<Missing Rack>".to_string());
            (target, "Toggle Rack Enable".to_string())
        }
        KeyBindingAction::ToggleRackSend {
            rack_id,
            target_rack_id,
        } => {
            let src_name = find_rack(*rack_id)
                .map(|r| r.name.clone())
                .unwrap_or_else(|| "<Missing Rack>".to_string());
            let dst_name = find_rack(*target_rack_id)
                .map(|r| r.name.clone())
                .unwrap_or_else(|| "<Missing Target>".to_string());
            (format!("{src_name} -> {dst_name}"), "Toggle Aux Send".to_string())
        }
        KeyBindingAction::ToggleMultiRackMute { rack_ids } => {
            (format_multi_rack_label(app, rack_ids), "Toggle Selected Mute".to_string())
        }
        KeyBindingAction::ToggleMultiRackSolo { rack_ids } => {
            (format_multi_rack_label(app, rack_ids), "Toggle Selected Solo".to_string())
        }
        KeyBindingAction::ToggleMultiRackEnabled { rack_ids } => {
            (format_multi_rack_label(app, rack_ids), "Toggle Selected Enable".to_string())
        }
        KeyBindingAction::LoadSessionFile { entry_id, path } => {
            let target = app
                .set_list
                .entries
                .iter()
                .find(|e| e.id == *entry_id || (!path.as_os_str().is_empty() && e.session_path == *path))
                .map(|e| e.title.clone())
                .or_else(|| {
                    path.file_name()
                        .and_then(|s| s.to_str())
                        .map(|s| s.to_string())
                })
                .unwrap_or_else(|| "<Missing Session File>".to_string());
            (target, "Switch Session / Settings".to_string())
        }
    }
}

fn format_multi_rack_label(app: &StageHostApp, rack_ids: &[Uuid]) -> String {
    let names: Vec<&str> = rack_ids
        .iter()
        .filter_map(|id| app.session.racks.iter().find(|r| r.id == *id).map(|r| r.name.as_str()))
        .collect();
    if names.is_empty() {
        format!("{} Racks (missing)", rack_ids.len())
    } else {
        format!("{} Racks: {}", names.len(), names.join(", "))
    }
}

fn draw_keybindings_modal(app: &mut StageHostApp, ctx: &egui::Context) {
    if !app.show_keybindings {
        return;
    }
    let mut open = app.show_keybindings;
    let mut discard_draft_on_close = false;

    let builder = egui::ViewportBuilder::default()
        .with_title("Key Bindings (Plugin & Rack Shortcuts) - StageHost")
        .with_inner_size([740.0, 540.0])
        .with_min_inner_size([620.0, 420.0]);

    ctx.show_viewport_immediate(keybindings_viewport_id(), builder, |ui, _class| {
        if ui.ctx().input(|i| i.viewport().close_requested()) {
            open = false;
        }
        if !app.handle_keybind_learn_input(ui.ctx())
            && ui.ctx().input(|i| i.key_pressed(egui::Key::Escape))
        {
            open = false;
        }

        egui::Panel::bottom("keybindings_footer")
            .frame(
                egui::Frame::new()
                    .fill(BG_PANEL)
                    .stroke(Stroke::new(1.0, BORDER_HAIRLINE))
                    .inner_margin(egui::Margin::symmetric(12, 8)),
            )
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    let has_pending_draft = !app.keybind_draft_combo.key.is_empty();
                    let footer_w = ui.available_width();
                    let btn_w = if has_pending_draft { 216.0 } else { 96.0 };
                    ui.allocate_ui_with_layout(
                        Vec2::new((footer_w - btn_w - 8.0).max(160.0), 24.0),
                        egui::Layout::left_to_right(egui::Align::Center),
                        |ui| {
                            let hint_txt = if has_pending_draft {
                                format!(
                                    "Pending key '{}' will be saved on Close",
                                    app.keybind_draft_combo.display_label()
                                )
                            } else {
                                "Key bindings save automatically with Session & Autosave".to_string()
                            };
                            ui.add(
                                egui::Label::new(
                                    RichText::new(hint_txt)
                                        .small()
                                        .color(if has_pending_draft { ACCENT_AMBER } else { TEXT_MUTED }),
                                )
                                .truncate(),
                            );
                        },
                    );
                    ui.allocate_ui_with_layout(
                        Vec2::new(btn_w, 24.0),
                        egui::Layout::right_to_left(egui::Align::Center),
                        |ui| {
                            if has_pending_draft {
                                if ui
                                    .add_sized(
                                        [108.0, 24.0],
                                        egui::Button::new(
                                            RichText::new("Save & Close").strong().color(TEXT_ON_ACCENT),
                                        )
                                        .fill(theme::FL_MINT_GREEN),
                                    )
                                    .clicked()
                                {
                                    open = false;
                                }
                                if ui
                                    .add_sized([92.0, 24.0], egui::Button::new("Discard"))
                                    .clicked()
                                {
                                    discard_draft_on_close = true;
                                    open = false;
                                }
                            } else if ui
                                .add_sized([84.0, 24.0], egui::Button::new("Close"))
                                .clicked()
                            {
                                open = false;
                            }
                        },
                    );
                });
            });

        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(BG_CHASSIS)
                    .inner_margin(egui::Margin::same(12)),
            )
            .show(ui, |ui| {
                egui::ScrollArea::vertical()
                    .id_salt("keybindings_scroll_body")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.add(
                            egui::Label::new(
                                RichText::new(
                                    "Assign keyboard shortcuts to bypass, enable, open GUI, or set parameters on VST3/CLAP/Built-In plugins.",
                                )
                                .small()
                                .color(TEXT_SECONDARY),
                            )
                            .wrap(),
                        );
                        ui.add_space(8.0);

                        draw_section_header(ui, "ACTIVE BINDINGS", None);
                        ui.add_space(4.0);
                        draw_keybindings_list(app, ui);

                        ui.add_space(12.0);
                        draw_section_header(ui, "ADD NEW BINDING", None);
                        ui.add_space(4.0);
                        draw_keybinding_creator(app, ui);
                    });
            });
    });

    if !open {
        if discard_draft_on_close {
            app.clear_keybind_draft_combo();
        } else if !app.keybind_draft_combo.key.is_empty() {
            app.commit_keybind_draft();
        }
        app.show_keybindings = false;
        app.keybind_learn_target_id = None;
        app.keybind_learn_draft = false;
    }
}

fn draw_keybindings_list(app: &mut StageHostApp, ui: &mut egui::Ui) {
    if app.session.key_bindings.is_empty() {
        egui::Frame::new()
            .fill(BG_WELL)
            .stroke(Stroke::new(1.0, BORDER_HAIRLINE))
            .corner_radius(CornerRadius::same(2))
            .inner_margin(10.0)
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.add(
                    egui::Label::new(
                        RichText::new(
                            "No key bindings configured yet. Select a target below, click 'Learn Key', and press a shortcut.",
                        )
                        .small()
                        .color(TEXT_MUTED),
                    )
                    .wrap(),
                );
            });
        return;
    }

    let descriptions: Vec<(String, String)> = app
        .session
        .key_bindings
        .iter()
        .map(|b| describe_key_binding(app, &b.action))
        .collect();
    let combos: Vec<KeyComboSpec> = app
        .session
        .key_bindings
        .iter()
        .map(|b| b.combo.clone())
        .collect();

    let mut remove_id: Option<Uuid> = None;
    let mut dirty = false;

    egui::Frame::new()
        .fill(BG_WELL)
        .stroke(Stroke::new(1.0, BORDER_HAIRLINE))
        .corner_radius(CornerRadius::same(2))
        .inner_margin(4.0)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            egui::ScrollArea::vertical()
                .id_salt("keybindings_list_scroll")
                .max_height(210.0)
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    for (idx, binding) in app.session.key_bindings.iter_mut().enumerate() {
                        let is_learning = app.keybind_learn_target_id == Some(binding.id);
                        let dup_count = combos
                            .iter()
                            .filter(|c| **c == binding.combo && !c.key.is_empty())
                            .count();
                        let builtin_conflict = is_builtin_shortcut(&binding.combo);
                        let (ref target_label, ref action_label) = descriptions[idx];

                        egui::Frame::new()
                            .fill(zebra_row_bg(idx))
                            .stroke(Stroke::new(1.0, BORDER_SUBTLE))
                            .inner_margin(egui::Margin::symmetric(6, 4))
                            .show(ui, |ui| {
                                ui.set_width(ui.available_width());
                                ui.horizontal(|ui| {
                                    dirty |= ui.checkbox(&mut binding.enabled, "").changed();
                                    let key_text = if is_learning {
                                        "Press key... (Esc)".to_string()
                                    } else {
                                        binding.combo.display_label()
                                    };
                                    let key_btn = if is_learning {
                                        egui::Button::new(
                                            RichText::new(key_text).strong().color(TEXT_ON_ACCENT),
                                        )
                                        .fill(ACCENT_AMBER)
                                        .min_size(Vec2::new(126.0, 20.0))
                                    } else {
                                        egui::Button::new(
                                            RichText::new(key_text).monospace().color(ACCENT_AMBER),
                                        )
                                        .min_size(Vec2::new(126.0, 20.0))
                                    };
                                    if ui
                                        .add(key_btn)
                                        .on_hover_text(
                                            "Click and press any key combination to rebind",
                                        )
                                        .clicked()
                                    {
                                        app.keybind_learn_draft = false;
                                        app.keybind_learn_target_id = if is_learning {
                                            None
                                        } else {
                                            Some(binding.id)
                                        };
                                    }

                                    let has_conflict = builtin_conflict || dup_count > 1;
                                    let reserved_tail = if has_conflict { 86.0 } else { 28.0 };
                                    let flex_w = (ui.available_width() - reserved_tail).max(160.0);
                                    let target_w = (flex_w * 0.56).floor();
                                    let action_w = (flex_w - target_w - 8.0).max(60.0);

                                    ui.allocate_ui_with_layout(
                                        Vec2::new(target_w, 20.0),
                                        egui::Layout::left_to_right(egui::Align::Center),
                                        |ui| {
                                            ui.add(
                                                egui::Label::new(
                                                    RichText::new(target_label)
                                                        .strong()
                                                        .color(TEXT_PRIMARY),
                                                )
                                                .truncate(),
                                            )
                                            .on_hover_text(target_label);
                                        },
                                    );
                                    ui.allocate_ui_with_layout(
                                        Vec2::new(action_w, 20.0),
                                        egui::Layout::left_to_right(egui::Align::Center),
                                        |ui| {
                                            ui.add(
                                                egui::Label::new(
                                                    RichText::new(action_label)
                                                        .small()
                                                        .color(TEXT_SECONDARY),
                                                )
                                                .truncate(),
                                            )
                                            .on_hover_text(action_label);
                                        },
                                    );
                                    if has_conflict {
                                        ui.label(
                                            RichText::new("Conflict")
                                                .small()
                                                .color(AUDIO_YELLOW),
                                        )
                                        .on_hover_text(
                                            "Shares a key combo with a built-in shortcut or another binding",
                                        );
                                    }
                                    if ui
                                        .small_button(RichText::new("x").color(AUDIO_RED))
                                        .on_hover_text("Delete binding")
                                        .clicked()
                                    {
                                        remove_id = Some(binding.id);
                                    }
                                });
                            });
                    }
                });
        });

    if let Some(id) = remove_id {
        app.push_undo_checkpoint();
        app.session.key_bindings.retain(|b| b.id != id);
        if app.keybind_learn_target_id == Some(id) {
            app.keybind_learn_target_id = None;
        }
        if !cfg!(test) {
            app.save_autosave_workspace();
        }
    } else if dirty {
        if !cfg!(test) {
            app.save_autosave_workspace();
        }
        app.set_status("Updated key binding state", false);
    }
}

fn draw_keybinding_creator(app: &mut StageHostApp, ui: &mut egui::Ui) {
    if app.session.racks.is_empty() && !app.keybind_draft_action.is_session_file_action() {
        if app.set_list.entries.is_empty() {
            ui.label(
                RichText::new("Add a rack or add a saved file to Session List before creating key bindings.")
                    .small()
                    .color(TEXT_MUTED),
            );
            return;
        }
        app.keybind_draft_action = KeybindDraftAction::LoadSessionFile;
    }

    if app
        .keybind_draft_rack_id
        .is_none_or(|rid| !app.session.racks.iter().any(|r| r.id == rid))
    {
        app.keybind_draft_rack_id = app.session.racks.first().map(|r| r.id);
    }
    if app
        .keybind_draft_session_entry_id
        .is_none_or(|eid| !app.set_list.entries.iter().any(|e| e.id == eid))
    {
        app.keybind_draft_session_entry_id = app
            .active_setlist_entry_id
            .filter(|eid| app.set_list.entries.iter().any(|e| e.id == *eid))
            .or_else(|| app.set_list.entries.first().map(|e| e.id));
    }

    let selected_rack_id = app.keybind_draft_rack_id;
    let selected_rack_name = selected_rack_id
        .and_then(|rid| app.session.racks.iter().find(|r| r.id == rid))
        .map(|r| r.name.clone())
        .unwrap_or_default();

    let slots_in_rack: Vec<(Uuid, String)> = selected_rack_id
        .and_then(|rid| app.session.racks.iter().find(|r| r.id == rid))
        .map(|r| r.slots.iter().map(|s| (s.id, s.name.clone())).collect())
        .unwrap_or_default();

    let sends_in_rack: Vec<(Uuid, String)> = selected_rack_id
        .and_then(|rid| app.session.racks.iter().find(|r| r.id == rid))
        .map(|r| {
            r.sends
                .iter()
                .map(|s| {
                    let dst_name = app
                        .session
                        .racks
                        .iter()
                        .find(|dst| dst.id == s.target_rack_id)
                        .map(|dst| dst.name.clone())
                        .unwrap_or_else(|| "<Missing Rack>".to_string());
                    (s.target_rack_id, dst_name)
                })
                .collect()
        })
        .unwrap_or_default();

    if app
        .keybind_draft_slot_id
        .is_none_or(|sid| !slots_in_rack.iter().any(|(id, _)| *id == sid))
    {
        app.keybind_draft_slot_id = slots_in_rack.first().map(|(id, _)| *id);
    }
    if app
        .keybind_draft_send_target_id
        .is_none_or(|tid| !sends_in_rack.iter().any(|(id, _)| *id == tid))
    {
        app.keybind_draft_send_target_id = sends_in_rack.first().map(|(id, _)| *id);
    }

    egui::Frame::new()
        .fill(BG_PANEL)
        .stroke(Stroke::new(1.0, BORDER_HAIRLINE))
        .corner_radius(CornerRadius::same(2))
        .inner_margin(8.0)
        .show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new("Action:").small().color(TEXT_MUTED));
                const DRAFT_ACTIONS: [KeybindDraftAction; 12] = [
                    KeybindDraftAction::ToggleSlotBypass,
                    KeybindDraftAction::ToggleSlotEnabled,
                    KeybindDraftAction::OpenSlotEditor,
                    KeybindDraftAction::SetSlotParam,
                    KeybindDraftAction::ToggleRackMute,
                    KeybindDraftAction::ToggleRackSolo,
                    KeybindDraftAction::ToggleRackEnabled,
                    KeybindDraftAction::ToggleRackSend,
                    KeybindDraftAction::ToggleMultiRackMute,
                    KeybindDraftAction::ToggleMultiRackSolo,
                    KeybindDraftAction::ToggleMultiRackEnabled,
                    KeybindDraftAction::LoadSessionFile,
                ];
                egui::ComboBox::from_id_salt("kb_action_combo")
                    .selected_text(app.keybind_draft_action.label())
                    .width(205.0)
                    .show_ui(ui, |ui| {
                        for act in DRAFT_ACTIONS {
                            if app.session.racks.is_empty() && !act.is_session_file_action() {
                                continue;
                            }
                            if ui
                                .selectable_label(app.keybind_draft_action == act, act.label())
                                .clicked()
                            {
                                app.keybind_draft_action = act;
                            }
                        }
                    });

                if !app.keybind_draft_action.is_session_file_action() {
                    if let Some(rack_id) = selected_rack_id {
                        ui.label(RichText::new("Rack:").small().color(TEXT_MUTED));
                        egui::ComboBox::from_id_salt("kb_rack_combo")
                            .selected_text(&selected_rack_name)
                            .width(130.0)
                            .show_ui(ui, |ui| {
                                for r in &app.session.racks {
                                    if ui
                                        .selectable_label(r.id == rack_id, &r.name)
                                        .clicked()
                                    {
                                        app.keybind_draft_rack_id = Some(r.id);
                                        app.keybind_draft_slot_id = r.slots.first().map(|s| s.id);
                                        app.keybind_draft_send_target_id =
                                            r.sends.first().map(|s| s.target_rack_id);
                                    }
                                }
                            });
                    }
                }
            });

            if app.keybind_draft_action.is_session_file_action() {
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new("Session File:").small().color(TEXT_MUTED));
                    if app.set_list.entries.is_empty() {
                        ui.label(
                            RichText::new("Session List is empty. Add saved setting/session files in the left Session List panel.")
                                .small()
                                .color(TEXT_MUTED),
                        );
                    } else if let Some(sel_eid) = app.keybind_draft_session_entry_id {
                        let sel_label = app
                            .set_list
                            .entries
                            .iter()
                            .find(|e| e.id == sel_eid)
                            .map(|e| e.title.clone())
                            .unwrap_or_default();
                        egui::ComboBox::from_id_salt("kb_session_entry_combo")
                            .selected_text(sel_label)
                            .width(240.0)
                            .show_ui(ui, |ui| {
                                for e in &app.set_list.entries {
                                    let kind = if is_settings_bundle_path(&e.session_path) {
                                        "Settings"
                                    } else {
                                        "Session"
                                    };
                                    let text = format!("{} [{}]", e.title, kind);
                                    if ui
                                        .selectable_label(e.id == sel_eid, text)
                                        .on_hover_text(e.session_path.display().to_string())
                                        .clicked()
                                    {
                                        app.keybind_draft_session_entry_id = Some(e.id);
                                    }
                                }
                            });
                    }
                });
            }

            if app.keybind_draft_action.is_multi_rack_action() {
                if app.keybind_draft_rack_ids.is_empty() && !app.selected_rack_ids.is_empty() {
                    app.keybind_draft_rack_ids = app.selected_rack_ids.clone();
                }
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new("Racks:").small().color(TEXT_MUTED));
                    for r in &app.session.racks {
                        let mut included = app.keybind_draft_rack_ids.contains(&r.id);
                        if ui.checkbox(&mut included, &r.name).changed() {
                            if included {
                                app.keybind_draft_rack_ids.push(r.id);
                            } else {
                                app.keybind_draft_rack_ids.retain(|id| *id != r.id);
                            }
                        }
                    }
                });
            }

            if app.keybind_draft_action.is_slot_action() {
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new("Plugin:").small().color(TEXT_MUTED));
                    if slots_in_rack.is_empty() {
                        ui.label(
                            RichText::new("Selected rack has no plugin slots.")
                                .small()
                                .color(TEXT_MUTED),
                        );
                    } else {
                        let sel_slot_id = app.keybind_draft_slot_id.expect("slot exists");
                        let sel_slot_name = slots_in_rack
                            .iter()
                            .find(|(id, _)| *id == sel_slot_id)
                            .map(|(_, n)| n.as_str())
                            .unwrap_or("");
                        egui::ComboBox::from_id_salt("kb_slot_combo")
                            .selected_text(sel_slot_name)
                            .width(200.0)
                            .show_ui(ui, |ui| {
                                for (sid, sname) in &slots_in_rack {
                                    if ui
                                        .selectable_label(*sid == sel_slot_id, sname)
                                        .clicked()
                                    {
                                        app.keybind_draft_slot_id = Some(*sid);
                                        if let Some(p) =
                                            app.slot_param_catalog.get(sid).and_then(|l| l.first())
                                        {
                                            app.keybind_draft_param_id = p.id;
                                            app.keybind_draft_param_value = p.value;
                                        }
                                    }
                                }
                            });
                    }
                });
            }

            if app.keybind_draft_action == KeybindDraftAction::SetSlotParam {
                if let Some(sid) = app.keybind_draft_slot_id {
                    let catalog = app.slot_param_catalog.get(&sid).map_or(&[][..], Vec::as_slice);
                    ui.horizontal_wrapped(|ui| {
                        ui.label(RichText::new("Param:").small().color(TEXT_MUTED));
                        if catalog.is_empty() {
                            ui.add(
                                egui::DragValue::new(&mut app.keybind_draft_param_id)
                                    .prefix("ID #"),
                            );
                            ui.add(
                                egui::Slider::new(&mut app.keybind_draft_param_value, 0.0..=1.0)
                                    .fixed_decimals(3),
                            );
                        } else {
                            let active_param = catalog
                                .iter()
                                .find(|p| p.id == app.keybind_draft_param_id)
                                .unwrap_or(&catalog[0]);
                            app.keybind_draft_param_id = active_param.id;
                            let mut min_v = active_param.min;
                            let mut max_v = active_param.max;
                            egui::ComboBox::from_id_salt("kb_param_combo")
                                .selected_text(&active_param.name)
                                .width(180.0)
                                .show_ui(ui, |ui| {
                                    for p in catalog {
                                        if ui
                                            .selectable_label(
                                                p.id == app.keybind_draft_param_id,
                                                &p.name,
                                            )
                                            .clicked()
                                        {
                                            app.keybind_draft_param_id = p.id;
                                            app.keybind_draft_param_value = p.value;
                                            min_v = p.min;
                                            max_v = p.max;
                                        }
                                    }
                                });
                            ui.add(
                                egui::Slider::new(
                                    &mut app.keybind_draft_param_value,
                                    min_v..=max_v,
                                )
                                .fixed_decimals(3),
                            );
                        }
                    });
                }
            }

            if app.keybind_draft_action.is_send_action() {
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new("Send Target:").small().color(TEXT_MUTED));
                    if sends_in_rack.is_empty() {
                        ui.label(
                            RichText::new("Selected rack has no Aux Sends configured.")
                                .small()
                                .color(TEXT_MUTED),
                        );
                    } else {
                        let sel_tid = app.keybind_draft_send_target_id.expect("send target exists");
                        let sel_tname = sends_in_rack
                            .iter()
                            .find(|(id, _)| *id == sel_tid)
                            .map(|(_, n)| n.as_str())
                            .unwrap_or("");
                        egui::ComboBox::from_id_salt("kb_send_target_combo")
                            .selected_text(format!("-> {sel_tname}"))
                            .width(200.0)
                            .show_ui(ui, |ui| {
                                for (tid, tname) in &sends_in_rack {
                                    if ui
                                        .selectable_label(*tid == sel_tid, format!("-> {tname}"))
                                        .clicked()
                                    {
                                        app.keybind_draft_send_target_id = Some(*tid);
                                    }
                                }
                            });
                    }
                });
            }

            ui.add_space(4.0);
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new("Shortcut:").small().color(TEXT_MUTED));
                let learn_text = if app.keybind_learn_draft {
                    "Press any key... (Esc to cancel)".to_string()
                } else if app.keybind_draft_combo.key.is_empty() {
                    "Click to Learn Key".to_string()
                } else {
                    format!("Key: {}", app.keybind_draft_combo.display_label())
                };
                let learn_btn = if app.keybind_learn_draft {
                    egui::Button::new(RichText::new(learn_text).strong().color(TEXT_ON_ACCENT))
                        .fill(ACCENT_AMBER)
                } else {
                    egui::Button::new(RichText::new(learn_text).monospace().color(ACCENT_AMBER))
                };
                if ui.add(learn_btn).clicked() {
                    app.keybind_learn_target_id = None;
                    app.keybind_learn_draft = !app.keybind_learn_draft;
                }

                let target_ready = if app.keybind_draft_action.is_session_file_action() {
                    app.keybind_draft_session_entry_id.is_some()
                } else if app.keybind_draft_action.is_slot_action() {
                    app.keybind_draft_slot_id.is_some()
                } else if app.keybind_draft_action.is_send_action() {
                    app.keybind_draft_send_target_id.is_some()
                } else if app.keybind_draft_action.is_multi_rack_action() {
                    !app.keybind_draft_rack_ids.is_empty()
                } else {
                    !app.session.racks.is_empty()
                };
                let key_ready = !app.keybind_draft_combo.key.is_empty();
                let add_btn = if target_ready && key_ready {
                    egui::Button::new(
                        RichText::new("+ Add Keybind")
                            .strong()
                            .color(TEXT_ON_ACCENT),
                    )
                    .fill(theme::FL_MINT_GREEN)
                } else {
                    egui::Button::new("+ Add Keybind")
                };
                if ui.add_enabled(target_ready && key_ready, add_btn).clicked() {
                    app.commit_keybind_draft();
                }

                if is_builtin_shortcut(&app.keybind_draft_combo) {
                    ui.label(
                        RichText::new("Conflicts with built-in shortcut")
                            .small()
                            .color(AUDIO_YELLOW),
                    );
                }
            });
        });
}

