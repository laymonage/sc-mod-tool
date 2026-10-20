use std::collections::HashSet;
use std::time::Duration;

use eframe::egui::{
    self, Align, Align2, CursorIcon, Key, Layout, Modifiers, Response, RichText, Role, Sense,
    WidgetInfo,
};
use egui_extras::{Column, TableBuilder};
use egui_material_icons::icons::{
    ICON_ARROW_DOWNWARD, ICON_ARROW_UPWARD, ICON_DOWNLOAD, ICON_FOLDER_OPEN, ICON_PAUSE,
    ICON_PLAY_ARROW, ICON_REFRESH, ICON_SEARCH, ICON_STOP, ICON_WARNING,
};
use sc_mod_tool_core::{
    Bank, Segment, SegmentSource,
    subtitles::{MappingConfidence, Subtitle},
};

const NOT_SHOWN_IN_GAME: &str = "Not shown in game";
const NO_TEXT: &str = "No text";
const NO_TEXT_HINT: &str =
    "This line exists in its scene, but the game's localization files have no text for it.";

use super::{SCModToolApp, ScanProgress, SortColumn, TextFilter};
use crate::audio::ActivePlayback;

/// Something the user asked for while the UI was drawn, applied afterwards.
enum Action {
    ChooseRoot,
    SelectLanguage(String),
    Rescan,
    CancelScan,
    SelectBank(usize),
    SelectSegment(usize),
    TogglePlayback(usize),
    PlayOrPause(usize),
    Export(usize),
    ExportBank,
    CancelBankExport,
    TogglePause,
    /// Move the seeker without seeking yet, or stop moving it.
    DragSeek(Option<f32>),
    Seek(Duration),
    ToggleWarnings,
    SortBy(SortColumn),
}

impl SCModToolApp {
    pub(super) fn show(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        let mut actions = Vec::new();
        self.handle_keyboard(&ctx, &mut actions);

        self.show_top_bar(ui, &mut actions);
        self.show_status_bar(ui, &mut actions);
        if self.root.is_none() {
            show_welcome(ui, &mut actions);
        } else if self.banks.is_empty() {
            self.show_loading(ui);
        } else {
            self.show_bank_panel(ui, &mut actions);
            self.show_inspector(ui, &mut actions);
            egui::CentralPanel::default().show(ui, |ui| self.show_segments(ui, &mut actions));
        }
        self.show_warnings_window(&ctx);

        // Show what the actions change, e.g. playback starting, which keeps
        // repainting from the next frame.
        if !actions.is_empty() {
            ctx.request_repaint();
        }
        for action in actions {
            self.apply(&ctx, action);
        }
    }

    fn apply(&mut self, ctx: &egui::Context, action: Action) {
        match action {
            Action::ChooseRoot => self.choose_root(ctx),
            Action::SelectLanguage(language) => self.select_language(ctx, language),
            Action::Rescan => self.rescan(ctx),
            Action::CancelScan => self.cancel_scan(),
            Action::SelectBank(index) => self.select_bank(index),
            Action::SelectSegment(index) => self.selected_segment = Some(index),
            Action::TogglePlayback(index) => {
                self.selected_segment = Some(index);
                self.toggle_playback(ctx, index);
            }
            Action::PlayOrPause(index) => {
                self.selected_segment = Some(index);
                self.play_or_pause(ctx, index);
            }
            Action::Export(index) => {
                self.selected_segment = Some(index);
                self.export_segment(ctx, index);
            }
            Action::ExportBank => self.export_bank(ctx),
            Action::CancelBankExport => self.cancel_bank_export(),
            Action::TogglePause => self.toggle_pause(),
            Action::DragSeek(position) => self.seek_drag = position,
            Action::Seek(position) => self.seek(position),
            Action::ToggleWarnings => self.show_warnings = !self.show_warnings,
            Action::SortBy(column) => {
                self.segment_sort = self.segment_sort.by(column);
                self.scroll_to_selection = true;
            }
        }
    }

    /// Arrow keys move through the segment list, and Space plays or pauses the
    /// selected segment.
    fn handle_keyboard(&mut self, ctx: &egui::Context, actions: &mut Vec<Action>) {
        if ctx.egui_wants_keyboard_input() || self.selected_bank().is_none() {
            return;
        }
        let (down, up, space) = ctx.input_mut(|input| {
            (
                input.consume_key(Modifiers::NONE, Key::ArrowDown),
                input.consume_key(Modifiers::NONE, Key::ArrowUp),
                input.consume_key(Modifiers::NONE, Key::Space),
            )
        });
        if down || up {
            let visible = self.visible_segments();
            let position = self
                .selected_segment
                .and_then(|selected| visible.iter().position(|index| *index == selected));
            let next = match (position, down) {
                (Some(position), true) => visible.get(position + 1),
                (Some(position), false) => position.checked_sub(1).and_then(|p| visible.get(p)),
                (None, _) => visible.first(),
            };
            if let Some(index) = next {
                actions.push(Action::SelectSegment(*index));
                self.scroll_to_selection = true;
            }
        }
        if space && let Some(index) = self.selected_segment {
            actions.push(Action::PlayOrPause(index));
        }
    }

    fn show_top_bar(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        let panel = egui::Panel::top("top-bar").frame(bar_frame(ui.style(), 8));
        panel.show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new("SC Mod Tool").strong().size(16.0));
                ui.separator();
                if ui
                    .button(format!("{} Open game folder…", ICON_FOLDER_OPEN.codepoint))
                    .clicked()
                {
                    actions.push(Action::ChooseRoot);
                }

                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if !self.languages.is_empty() {
                        let scanning = self.scan_progress.is_some();
                        if ui
                            .add_enabled(
                                !scanning,
                                egui::Button::new(format!("{} Rescan", ICON_REFRESH.codepoint)),
                            )
                            .on_hover_text("Read the game files again, including its scripts.")
                            .clicked()
                        {
                            actions.push(Action::Rescan);
                        }
                        let selected = self
                            .languages
                            .iter()
                            .find(|language| {
                                Some(&language.directory) == self.selected_language.as_ref()
                            })
                            .map_or("Select…", |language| language.label.as_str());
                        egui::ComboBox::from_id_salt("language")
                            .selected_text(selected)
                            .show_ui(ui, |ui| {
                                for language in &self.languages {
                                    let is_selected = Some(&language.directory)
                                        == self.selected_language.as_ref();
                                    if ui.selectable_label(is_selected, &language.label).clicked() {
                                        actions.push(Action::SelectLanguage(
                                            language.directory.clone(),
                                        ));
                                    }
                                }
                            })
                            .response
                            .on_hover_cursor(CursorIcon::PointingHand);
                        ui.label("Language");
                        ui.separator();
                    }
                    if let Some(root) = &self.root {
                        ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                            ui.add(
                                egui::Label::new(RichText::new(root.display().to_string()).weak())
                                    .truncate(),
                            );
                        });
                    }
                });
            });
        });
    }

    fn show_status_bar(&self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        let panel = egui::Panel::bottom("status-bar").frame(bar_frame(ui.style(), 4));
        panel.show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if !self.subtitle_warnings.is_empty()
                        && ui
                            .button(
                                RichText::new(format!(
                                    "{} {} warnings",
                                    ICON_WARNING.codepoint,
                                    self.subtitle_warnings.len()
                                ))
                                .color(ui.visuals().warn_fg_color),
                            )
                            .clicked()
                    {
                        actions.push(Action::ToggleWarnings);
                    }
                    if let Some(id) = self.loaded_segment_id() {
                        let label = self.find_segment(id).map_or_else(
                            || id.to_owned(),
                            |(bank, segment)| segment_label(bank, segment),
                        );
                        if let Some(playback) = &self.active_playback {
                            self.show_playback_controls(ui, playback, actions);
                        }
                        ui.label(label);
                        if self.active_playback.is_none() {
                            // Still being decoded.
                            ui.spinner();
                        }
                        ui.separator();
                    }
                    if let Some(export) = &self.bank_export {
                        if export.is_cancelling() {
                            ui.label("Cancelling export…");
                        } else {
                            if ui.button("Cancel").clicked() {
                                actions.push(Action::CancelBankExport);
                            }
                            ui.add(
                                egui::ProgressBar::new(
                                    export.completed as f32 / export.total.max(1) as f32,
                                )
                                .desired_width(180.0)
                                .text(format!("{} / {} segments", export.completed, export.total)),
                            );
                            ui.label(format!("Exporting {}", export.label));
                        }
                        ui.spinner();
                        ui.separator();
                    }
                    // The status takes whatever space the controls leave.
                    ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                        if let Some(progress) = &self.scan_progress {
                            ui.spinner();
                            match progress {
                                ScanProgress::Banks {
                                    completed,
                                    total,
                                    bank,
                                } if !self.is_cancelling_scan() => {
                                    let fraction = if *total == 0 {
                                        0.0
                                    } else {
                                        *completed as f32 / *total as f32
                                    };
                                    ui.add(
                                        egui::ProgressBar::new(fraction)
                                            .desired_width(180.0)
                                            .text(format!("{completed} / {total} banks")),
                                    );
                                    if ui.button("Cancel").clicked() {
                                        actions.push(Action::CancelScan);
                                    }
                                    ui.add(egui::Label::new(RichText::new(bank).weak()).truncate());
                                }
                                _ => {
                                    ui.add(egui::Label::new(self.scan_activity()).truncate());
                                    if !self.is_cancelling_scan() && ui.button("Cancel").clicked() {
                                        actions.push(Action::CancelScan);
                                    }
                                }
                            }
                        } else if self.status.is_error {
                            ui.add(
                                egui::Label::new(
                                    RichText::new(format!(
                                        "{} {}",
                                        ICON_WARNING.codepoint, self.status.message
                                    ))
                                    .color(ui.visuals().error_fg_color),
                                )
                                .truncate(),
                            );
                        } else {
                            ui.add(egui::Label::new(&self.status.message).truncate());
                        }
                    });
                });
            });
        });
    }

    /// The seeker, its time readout and the pause button, laid out right to
    /// left.
    fn show_playback_controls(
        &self,
        ui: &mut egui::Ui,
        playback: &ActivePlayback,
        actions: &mut Vec<Action>,
    ) {
        let total = playback.duration.as_secs_f32();
        let mut position = self
            .seek_drag
            .unwrap_or_else(|| playback.position().as_secs_f32());
        ui.label(
            RichText::new(format!(
                "{} / {}",
                format_time(position),
                format_time(total)
            ))
            .monospace(),
        );
        ui.spacing_mut().slider_width = 160.0;
        let response = ui
            .add(egui::Slider::new(&mut position, 0.0..=total).show_value(false))
            .on_hover_and_drag_cursor(CursorIcon::PointingHand);
        // The slider has no visible label, so it's named for screen readers.
        response.widget_info(|| WidgetInfo::slider(true, f64::from(position), "Seek"));
        if response.dragged() {
            actions.push(Action::DragSeek(Some(position)));
        } else if response.drag_stopped() || response.changed() {
            actions.push(Action::Seek(Duration::from_secs_f32(position)));
        }
        let (pause_icon, name) = if playback.player.is_paused() {
            (ICON_PLAY_ARROW, "Play")
        } else {
            (ICON_PAUSE, "Pause")
        };
        if name_icon_button(ui.button(pause_icon.codepoint), name).clicked() {
            actions.push(Action::TogglePause);
        }
    }

    fn show_loading(&self, ui: &mut egui::Ui) {
        egui::CentralPanel::default().show(ui, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(ui.available_height() * 0.35);
                if self.discovering_languages || self.scan_progress.is_some() {
                    ui.spinner();
                    ui.label(self.scan_activity());
                } else {
                    ui.label(RichText::new("No sound banks to show").heading());
                    ui.label(
                        RichText::new(
                            "Check the status bar below, then open another folder or rescan.",
                        )
                        .weak(),
                    );
                }
            });
        });
    }

    fn show_bank_panel(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        egui::Panel::left("banks")
            .resizable(true)
            .default_size(220.0)
            .size_range(160.0..=420.0)
            .show(ui, |ui| {
                ui.add_space(6.0);
                ui.label(RichText::new("Sound banks").strong());
                ui.add_space(4.0);
                ui.add(
                    egui::TextEdit::singleline(&mut self.bank_filter)
                        .hint_text(format!("{} Filter", ICON_SEARCH.codepoint))
                        .desired_width(f32::INFINITY),
                );
                ui.add_space(4.0);

                let filter = self.bank_filter.to_lowercase();
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.with_layout(Layout::top_down_justified(Align::LEFT), |ui| {
                            let mut shown = 0;
                            let mut shown_group: Option<&str> = None;
                            for (index, bank) in self.banks.iter().enumerate() {
                                if ![
                                    Some(&bank.name),
                                    bank.map.as_ref(),
                                    bank.mission_name.as_ref(),
                                ]
                                .into_iter()
                                .flatten()
                                .any(|text| text.to_lowercase().contains(&filter))
                                {
                                    continue;
                                }
                                shown += 1;
                                let group = mission_label(bank).unwrap_or("Other");
                                if shown_group != Some(group) {
                                    ui.add_space(6.0);
                                    ui.label(RichText::new(group).small().weak());
                                    shown_group = Some(group);
                                }
                                if bank_row(ui, bank, self.selected_bank == Some(index)).clicked() {
                                    actions.push(Action::SelectBank(index));
                                }
                            }
                            if shown == 0 {
                                ui.label(RichText::new("No banks match.").weak());
                            }
                        });
                    });
            });
    }

    fn show_segments(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        let Some(bank_index) = self.selected_bank else {
            ui.centered_and_justified(|ui| {
                ui.label(RichText::new("Select a sound bank on the left.").weak());
            });
            return;
        };
        let bank = &self.banks[bank_index];

        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new(&bank.name).heading());
            let mission = mission_label(bank)
                .map(|mission| format!("{mission} · "))
                .unwrap_or_default();
            let contents = if bank.sound_kind.is_some() {
                // A map's banks share its file, so its size isn't theirs.
                format!("{} sounds · {}", bank.segments.len(), bank.path)
            } else {
                format!(
                    "{} segments · {}",
                    bank.segments.len(),
                    format_bytes(bank.size)
                )
            };
            ui.label(RichText::new(format!("{mission}{contents}")).weak());
            if bank.issue.is_none() && !bank.segments.is_empty() {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let exporting = self.bank_export.is_some();
                    let button = ui
                        .add_enabled(
                            !exporting,
                            egui::Button::new(format!("{} Export bank…", ICON_DOWNLOAD.codepoint)),
                        )
                        .on_hover_text("Save every segment as a WAV file.")
                        .on_disabled_hover_text("Another bank is being exported.");
                    if button.clicked() {
                        actions.push(Action::ExportBank);
                    }
                });
            }
        });
        if let Some(issue) = &bank.issue {
            ui.add_space(8.0);
            egui::Frame::group(ui.style()).show(ui, |ui| {
                ui.label(
                    RichText::new(format!(
                        "{} This bank can't be played",
                        ICON_WARNING.codepoint
                    ))
                    .strong()
                    .color(ui.visuals().warn_fg_color),
                );
                ui.label(issue);
            });
            return;
        }

        ui.add_space(4.0);
        let visible = self.visible_segments();
        // Clear a selection the filters hide instead of moving it, so typing a
        // search doesn't change what's selected.
        if self
            .selected_segment
            .is_some_and(|selected| !visible.contains(&selected))
        {
            self.selected_segment = None;
            ui.ctx().request_repaint();
        }
        // Map sounds have names, but no subtitles.
        let is_map_bank = bank.sound_kind.is_some();
        let has_subtitles = self.has_subtitles(bank);
        ui.horizontal(|ui| {
            let hint = match (is_map_bank, has_subtitles) {
                (false, _) => "Search subtitles and cues",
                (true, false) => "Search names, keys and levels",
                (true, true) => "Search names, keys, levels and subtitles",
            };
            let hint = format!("{} {hint}", ICON_SEARCH.codepoint);
            ui.add(
                egui::TextEdit::singleline(&mut self.segment_filter)
                    .hint_text(hint)
                    .desired_width(260.0),
            );
            if has_subtitles {
                egui::ComboBox::from_id_salt("text-filter")
                    .selected_text(self.text_filter.label())
                    .show_ui(ui, |ui| {
                        for filter in TextFilter::ALL {
                            ui.selectable_value(&mut self.text_filter, filter, filter.label());
                        }
                    })
                    .response
                    .on_hover_cursor(CursorIcon::PointingHand);
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.label(
                    RichText::new(format!("{} of {}", visible.len(), bank.segments.len())).weak(),
                );
            });
        });
        ui.add_space(4.0);

        let scroll_to_selection = std::mem::take(&mut self.scroll_to_selection);
        let playing = self.playing_segment_id();
        let row_height = ui.spacing().interact_size.y + 4.0;
        let columns = table_columns(is_map_bank, has_subtitles);
        let mut table = TableBuilder::new(ui)
            .id_salt("segments")
            .striped(true)
            .resizable(true)
            .sense(Sense::click())
            .auto_shrink([false, false])
            .cell_layout(Layout::left_to_right(Align::Center));
        for column in &columns {
            table = table.column(column.width);
        }
        if scroll_to_selection
            && let Some(row) = self
                .selected_segment
                .and_then(|selected| visible.iter().position(|index| *index == selected))
        {
            table = table.scroll_to_row(row, None);
        }

        let sort = self.segment_sort;
        let map_subtitles = is_map_bank && has_subtitles;
        table
            .header(22.0, |mut header| {
                for column in &columns {
                    header.col(|ui| {
                        let Some(sort_column) = column.sort else {
                            ui.strong(column.title);
                            return;
                        };
                        let title = if sort.column == sort_column {
                            let arrow = if sort.descending {
                                ICON_ARROW_DOWNWARD
                            } else {
                                ICON_ARROW_UPWARD
                            };
                            format!("{} {}", column.title, arrow.codepoint)
                        } else {
                            column.title.to_owned()
                        };
                        if ui
                            .add(egui::Button::new(RichText::new(title).strong()).frame(false))
                            .on_hover_text("Sort by this column; click again to reverse")
                            .on_hover_cursor(CursorIcon::PointingHand)
                            .clicked()
                        {
                            actions.push(Action::SortBy(sort_column));
                        }
                    });
                }
            })
            .body(|body| {
                body.rows(row_height, visible.len(), |mut row| {
                    let index = visible[row.index()];
                    let segment = &bank.segments[index];
                    let is_playing = playing == Some(segment.id.as_str());
                    row.set_selected(self.selected_segment == Some(index));

                    row.col(|ui| {
                        let (icon, name) = if is_playing {
                            (ICON_STOP, "Stop")
                        } else {
                            (ICON_PLAY_ARROW, "Play")
                        };
                        // Fill the cell, to centre the icon in it.
                        ui.centered_and_justified(|ui| {
                            let button = ui.add(egui::Button::new(icon.codepoint).frame(false));
                            if name_icon_button(button, name).clicked() {
                                actions.push(Action::TogglePlayback(index));
                            }
                        });
                    });
                    row.col(|ui| {
                        ui.label(format!("{}", segment.segment + 1));
                    });
                    row.col(|ui| {
                        ui.label(format_duration(segment));
                    });
                    match segment.source {
                        SegmentSource::MapSound {
                            subsong,
                            key,
                            ref areas,
                            ..
                        } => {
                            row.col(|ui| {
                                ui.label(areas.len().to_string())
                                    .on_hover_text("How many of the map's areas play it");
                            });
                            row.col(|ui| {
                                ui.label(subsong.to_string())
                                    .on_hover_text("The map entry that plays it");
                            });
                            row.col(|ui| {
                                ui.label(RichText::new(format_key(key)).monospace());
                            });
                            row.col(|ui| {
                                ui.add(
                                    egui::Label::new(segment.name.as_deref().unwrap_or_default())
                                        .truncate(),
                                );
                            });
                            if map_subtitles {
                                row.col(|ui| subtitle_cell(ui, self.subtitles.get(&segment.id)));
                            }
                        }
                        SegmentSource::Stream {
                            offset,
                            byte_length,
                        } => {
                            row.col(|ui| {
                                ui.label(format_bytes(byte_length));
                            });
                            row.col(|ui| {
                                ui.label(RichText::new(offset.to_string()).monospace());
                            });
                            row.col(|ui| subtitle_cell(ui, self.subtitles.get(&segment.id)));
                        }
                    }

                    let response = row.response().on_hover_cursor(CursorIcon::PointingHand);
                    if response.double_clicked() {
                        actions.push(Action::TogglePlayback(index));
                    } else if response.clicked() {
                        actions.push(Action::SelectSegment(index));
                    }
                });
            });
    }

    fn show_inspector(&self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        egui::Panel::right("inspector")
            .resizable(true)
            .default_size(320.0)
            .size_range(240.0..=520.0)
            .show(ui, |ui| {
                let (Some(bank), Some(row), Some(segment)) = (
                    self.selected_bank(),
                    self.selected_segment,
                    self.selected_segment(),
                ) else {
                    ui.centered_and_justified(|ui| {
                        ui.label(RichText::new("Select a segment to see its details.").weak());
                    });
                    return;
                };

                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.add_space(6.0);
                        let noun = if bank.sound_kind.is_some() {
                            "Sound"
                        } else {
                            "Segment"
                        };
                        ui.label(
                            RichText::new(format!("{noun} {}", segment.segment + 1)).heading(),
                        );
                        ui.label(RichText::new(bank_title(bank)).weak());
                        ui.add_space(8.0);

                        // Segment actions. Replace will go here.
                        ui.horizontal(|ui| {
                            let is_playing =
                                self.active_playback.as_ref().is_some_and(|playback| {
                                    playback.segment_id == segment.id
                                        && !playback.player.is_paused()
                                });
                            let (icon, label) = if is_playing {
                                (ICON_PAUSE, "Pause")
                            } else {
                                (ICON_PLAY_ARROW, "Play")
                            };
                            if action_button(ui, icon.codepoint, label)
                                .on_hover_text("Space")
                                .clicked()
                            {
                                actions.push(Action::PlayOrPause(row));
                            }
                            if action_button(ui, ICON_DOWNLOAD.codepoint, "Export…")
                                .on_hover_text("Save as a WAV file.")
                                .clicked()
                            {
                                actions.push(Action::Export(row));
                            }
                        });

                        ui.add_space(8.0);
                        ui.separator();
                        ui.label(RichText::new("Details").strong());
                        egui::Grid::new("segment-details")
                            .num_columns(2)
                            .spacing([16.0, 4.0])
                            .show(ui, |ui| {
                                detail_row(ui, "Duration", format_duration(segment));
                                detail_row(
                                    ui,
                                    "Sample rate",
                                    format!("{} Hz", segment.sample_rate),
                                );
                                detail_row(ui, "Channels", format_channels(segment.channels));
                                detail_row(ui, "Format", segment.codec.clone());
                                match segment.source {
                                    SegmentSource::Stream {
                                        offset,
                                        byte_length,
                                    } => {
                                        detail_row(
                                            ui,
                                            "Size",
                                            format!(
                                                "{} ({byte_length} bytes)",
                                                format_bytes(byte_length)
                                            ),
                                        );
                                        detail_row(ui, "Offset", offset.to_string());
                                    }
                                    SegmentSource::MapSound { subsong, key, .. } => {
                                        if let Some(name) = &segment.name {
                                            detail_row(ui, "Name", name.clone());
                                        }
                                        detail_row(ui, "Key", format_key(key));
                                        detail_row(ui, "Map entry", subsong.to_string());
                                    }
                                }
                                detail_row(ui, "File", bank.path.clone());
                                detail_row(ui, "ID", segment.id.clone());
                            });

                        // Map sounds list where they're played, and only the
                        // few that are dialogue have subtitles.
                        if let SegmentSource::MapSound { areas, .. } = &segment.source {
                            ui.add_space(8.0);
                            ui.separator();
                            ui.label(RichText::new("Used in").strong());
                            for area in areas {
                                ui.label(area);
                            }
                            if !self.subtitles.contains_key(&segment.id) {
                                return;
                            }
                        }
                        ui.add_space(8.0);
                        ui.separator();
                        let subtitles = self
                            .subtitles
                            .get(&segment.id)
                            .map(Vec::as_slice)
                            .unwrap_or_default();
                        ui.label(RichText::new("Subtitles").strong());
                        if subtitles.is_empty() {
                            ui.label(
                                RichText::new("No subtitle is linked to this segment.").weak(),
                            );
                        }
                        for (position, subtitle) in subtitles.iter().enumerate() {
                            subtitle_card(ui, position, subtitle);
                        }
                    });
            });
    }

    fn show_warnings_window(&mut self, ctx: &egui::Context) {
        let warnings = &self.subtitle_warnings;
        egui::Window::new("Warnings")
            .open(&mut self.show_warnings)
            .pivot(Align2::CENTER_CENTER)
            .default_pos(ctx.content_rect().center())
            .default_size([640.0, 420.0])
            .show(ctx, |ui| {
                ui.label(
                    RichText::new(
                        "Script references that could not be matched to a segment and subtitle.",
                    )
                    .weak(),
                );
                ui.separator();
                egui::ScrollArea::vertical().show(ui, |ui| {
                    for warning in warnings {
                        ui.label(warning);
                    }
                });
            });
    }

    /// The selected bank's segments that match the search and filters.
    pub(super) fn visible_segments(&self) -> Vec<usize> {
        let Some(bank) = self.selected_bank() else {
            return Vec::new();
        };
        let query = self.segment_filter.trim().to_lowercase();
        // The text filter is only offered for banks with subtitles.
        let filters_text = self.has_subtitles(bank);
        let visible = bank
            .segments
            .iter()
            .enumerate()
            .filter(|(_, segment)| {
                let subtitles = self.subtitles.get(&segment.id);
                let has_text =
                    subtitles.is_some_and(|subtitles| subtitles.iter().any(Subtitle::has_text));
                let shown = !filters_text
                    || match self.text_filter {
                        TextFilter::All => true,
                        TextFilter::WithText => has_text,
                        TextFilter::WithoutText => !has_text,
                    };
                let subtitle_matches = subtitles.is_some_and(|subtitles| {
                    subtitles.iter().any(|subtitle| {
                        subtitle
                            .text
                            .as_deref()
                            .is_some_and(|text| text.to_lowercase().contains(&query))
                            || subtitle.cue.to_lowercase().contains(&query)
                    })
                });
                // Map sounds can also be found by name, key and level.
                let sound_matches = match &segment.source {
                    SegmentSource::MapSound { key, areas, .. } => {
                        segment
                            .name
                            .as_deref()
                            .is_some_and(|name| name.to_lowercase().contains(&query))
                            || format_key(*key).to_lowercase().contains(&query)
                            || areas
                                .iter()
                                .any(|area| area.to_lowercase().contains(&query))
                    }
                    SegmentSource::Stream { .. } => false,
                };
                shown && (query.is_empty() || subtitle_matches || sound_matches)
            })
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        // Banks without the sorted column keep their own order.
        let sort = self.segment_sort;
        if !table_columns(bank.sound_kind.is_some(), filters_text)
            .iter()
            .any(|column| column.sort == Some(sort.column))
        {
            return visible;
        }
        let mut keyed = visible
            .into_iter()
            .map(|index| (self.sort_key(&bank.segments[index], sort.column), index))
            .collect::<Vec<_>>();
        // Missing values go last either way, and segments with equal values
        // stay in the bank's order.
        keyed.sort_by(
            |((left_missing, left), left_index), ((right_missing, right), right_index)| {
                let values = if sort.descending {
                    right.cmp(left)
                } else {
                    left.cmp(right)
                };
                left_missing
                    .cmp(right_missing)
                    .then(values)
                    .then(left_index.cmp(right_index))
            },
        );
        keyed.into_iter().map(|(_, index)| index).collect()
    }

    /// A segment's value in `column`, after a flag that's set when it has
    /// none, such as a stream's key.
    fn sort_key(&self, segment: &Segment, column: SortColumn) -> (bool, SortValue) {
        let number = |value: Option<u64>| (value.is_none(), SortValue::Number(value.unwrap_or(0)));
        let text = |value: Option<String>| {
            (
                value.is_none(),
                SortValue::Text(value.unwrap_or_default().to_lowercase()),
            )
        };
        let (subsong, key, areas) = match &segment.source {
            SegmentSource::MapSound {
                subsong,
                key,
                areas,
                ..
            } => (Some(*subsong), *key, Some(areas.len())),
            SegmentSource::Stream { .. } => (None, None, None),
        };
        match column {
            SortColumn::Number => number(Some(segment.segment as u64)),
            SortColumn::Duration => number(Some(
                u64::from(segment.sample_count) * 1_000_000 / u64::from(segment.sample_rate.max(1)),
            )),
            SortColumn::Size => number(segment.stream_range().map(|(_, length)| length)),
            SortColumn::Offset => number(segment.stream_range().map(|(offset, _)| offset)),
            SortColumn::UsedIn => number(areas.map(|count| count as u64)),
            SortColumn::Entry => number(subsong.map(u64::from)),
            SortColumn::Key => number(key.map(u64::from)),
            SortColumn::Name => text(segment.name.clone()),
            // As shown: the first subtitle's text, or its cue when it has none.
            SortColumn::Subtitle => text(self.subtitles.get(&segment.id).map(|subtitles| {
                subtitles[0]
                    .text
                    .clone()
                    .unwrap_or_else(|| subtitles[0].cue.clone())
            })),
        }
    }

    /// Whether any of the bank's segments has a subtitle. `.LS0` banks are
    /// treated as having them, since that's what they're for.
    fn has_subtitles(&self, bank: &Bank) -> bool {
        bank.sound_kind.is_none()
            || bank
                .segments
                .iter()
                .any(|segment| self.subtitles.contains_key(&segment.id))
    }

    fn find_segment(&self, id: &str) -> Option<(&Bank, &Segment)> {
        self.banks.iter().find_map(|bank| {
            bank.segments
                .iter()
                .find(|segment| segment.id == id)
                .map(|segment| (bank, segment))
        })
    }
}

fn show_welcome(ui: &mut egui::Ui, actions: &mut Vec<Action>) {
    egui::CentralPanel::default().show(ui, |ui| {
        ui.vertical_centered(|ui| {
            ui.add_space(ui.available_height() * 0.3);
            ui.label(
                RichText::new("Splinter Cell sound banks")
                    .size(24.0)
                    .strong(),
            );
            ui.add_space(6.0);
            ui.label("Browse and play the game's voice and sound banks, with their subtitles.");
            ui.add_space(16.0);
            if ui
                .add(egui::Button::new(
                    RichText::new(format!("{}  Open game folder…", ICON_FOLDER_OPEN.codepoint))
                        .size(16.0),
                ))
                .clicked()
            {
                actions.push(Action::ChooseRoot);
            }
            ui.add_space(8.0);
            ui.label(
                RichText::new("Choose the game's installation folder, or its Sounds folder.")
                    .weak(),
            );
            ui.label(RichText::new("Game files are only read, never changed.").weak());
        });
    });
}

fn bank_row(ui: &mut egui::Ui, bank: &Bank, selected: bool) -> egui::Response {
    let name = if bank.segments.is_empty() {
        RichText::new(&bank.name).weak()
    } else {
        RichText::new(&bank.name)
    };
    let response = ui.selectable_label(selected, name);
    let (count, hint) = if bank.segments.is_empty() {
        (ICON_WARNING.codepoint.to_owned(), bank.issue.as_deref())
    } else {
        (bank.segments.len().to_string(), None)
    };
    ui.painter().text(
        response.rect.right_center() - egui::vec2(6.0, 0.0),
        Align2::RIGHT_CENTER,
        count,
        egui::TextStyle::Small.resolve(ui.style()),
        ui.visuals().weak_text_color(),
    );
    match hint {
        Some(hint) => response.on_hover_text(hint),
        None => response,
    }
}

/// A column of the segment table.
struct TableColumn {
    title: &'static str,
    sort: Option<SortColumn>,
    width: Column,
}

/// The segment table's columns for an `.LS0` bank or a sound map's bank.
/// Map sounds' names come before their subtitles, when they have any.
fn table_columns(is_map_bank: bool, has_subtitles: bool) -> Vec<TableColumn> {
    let column = |title, sort, width| TableColumn { title, sort, width };
    let mut columns = vec![
        column("", None, Column::exact(28.0)),
        column("#", Some(SortColumn::Number), Column::auto().at_least(36.0)),
        column(
            "Duration",
            Some(SortColumn::Duration),
            Column::auto().at_least(64.0),
        ),
    ];
    if is_map_bank {
        columns.extend([
            column(
                "Used in",
                Some(SortColumn::UsedIn),
                Column::auto().at_least(64.0),
            ),
            column(
                "Entry",
                Some(SortColumn::Entry),
                Column::auto().at_least(56.0),
            ),
            column("Key", Some(SortColumn::Key), Column::auto().at_least(80.0)),
        ]);
        if has_subtitles {
            columns.push(column(
                "Name",
                Some(SortColumn::Name),
                Column::auto().at_least(120.0).clip(true),
            ));
            columns.push(column(
                "Subtitle",
                Some(SortColumn::Subtitle),
                Column::remainder().clip(true),
            ));
        } else {
            columns.push(column(
                "Name",
                Some(SortColumn::Name),
                Column::remainder().clip(true),
            ));
        }
    } else {
        columns.extend([
            column(
                "Size",
                Some(SortColumn::Size),
                Column::auto().at_least(64.0),
            ),
            column(
                "Offset",
                Some(SortColumn::Offset),
                Column::auto().at_least(80.0),
            ),
            column(
                "Subtitle",
                Some(SortColumn::Subtitle),
                Column::remainder().clip(true),
            ),
        ]);
    }
    columns
}

/// A value segments are sorted by.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
enum SortValue {
    Number(u64),
    Text(String),
}

/// A segment's first subtitle, for the segment table.
fn subtitle_cell(ui: &mut egui::Ui, subtitles: Option<&Vec<Subtitle>>) {
    let Some(subtitles) = subtitles else {
        ui.label(RichText::new("—").weak());
        return;
    };
    // Subtitles with text are sorted first, so if the first has none, none of
    // them do.
    let Some(mut text) = subtitles[0].text.clone() else {
        ui.add(
            egui::Label::new(
                RichText::new(format!("{NO_TEXT} · {}", subtitles[0].cue))
                    .italics()
                    .weak(),
            )
            .truncate(),
        )
        .on_hover_text(NO_TEXT_HINT);
        return;
    };
    if subtitles.len() > 1 {
        text.push_str(&format!("  (+{} more)", subtitles.len() - 1));
    }
    // Subtitles the game shows are sorted next, so this means none of them
    // are shown.
    if is_shown_in_game(&subtitles[0]) {
        ui.add(egui::Label::new(text).truncate());
    } else {
        ui.add(egui::Label::new(RichText::new(text).italics().weak()).truncate())
            .on_hover_text(NOT_SHOWN_IN_GAME);
    }
}

fn is_shown_in_game(subtitle: &Subtitle) -> bool {
    subtitle.confidence == MappingConfidence::ExplicitCompiledReference
}

fn subtitle_card(ui: &mut egui::Ui, position: usize, subtitle: &Subtitle) {
    egui::Frame::group(ui.style()).show(ui, |ui| {
        ui.set_width(ui.available_width());
        // Read-only for now; editing subtitles will make this editable.
        let mut text = subtitle.text.as_deref().unwrap_or_default();
        ui.add(
            egui::TextEdit::multiline(&mut text)
                .hint_text("No text yet")
                .desired_rows(2)
                .desired_width(f32::INFINITY),
        );
        ui.horizontal(|ui| {
            ui.label(RichText::new(&subtitle.cue).small().weak());
            if !subtitle.has_text() {
                ui.label(RichText::new(NO_TEXT).small().italics().weak())
                    .on_hover_text(NO_TEXT_HINT);
            }
            if !is_shown_in_game(subtitle) {
                ui.label(RichText::new(NOT_SHOWN_IN_GAME).small().italics().weak())
                    .on_hover_text("The game plays this line without a subtitle. Its text is the line at the same position in the scene's localization section.");
            }
        });
        egui::CollapsingHeader::new(RichText::new("Where it comes from").small())
            .id_salt(("subtitle-source", position))
            .show(ui, |ui| {
                egui::Grid::new(("subtitle-source-grid", position))
                    .num_columns(2)
                    .spacing([12.0, 2.0])
                    .show(ui, |ui| {
                        detail_row(ui, "Sound", subtitle.sound_event.clone());
                        detail_row(ui, "Map key", format!("0x{:08X}", subtitle.sound_map_key));
                        ui.label(RichText::new("Script").weak());
                        ui.vertical(|ui| {
                            for source in &subtitle.sources {
                                ui.label(RichText::new(source).small());
                            }
                        });
                        ui.end_row();
                    });
            })
            .header_response
            .on_hover_cursor(CursorIcon::PointingHand);
    });
    ui.add_space(4.0);
}

fn detail_row(ui: &mut egui::Ui, name: &str, value: String) {
    ui.label(RichText::new(name).weak());
    ui.label(value);
    ui.end_row();
}

/// A top or bottom bar's frame, with the same space above and below its
/// contents.
fn bar_frame(style: &egui::Style, vertical_margin: i8) -> egui::Frame {
    egui::Frame::side_top_panel(style).inner_margin(egui::Margin::symmetric(8, vertical_margin))
}

/// Formats seconds as `m:ss`.
fn format_time(seconds: f32) -> String {
    let seconds = seconds.max(0.0) as u32;
    format!("{}:{:02}", seconds / 60, seconds % 60)
}

pub(super) fn segment_label(bank: &Bank, segment: &Segment) -> String {
    format!("{} #{}", bank_title(bank), segment.segment + 1)
}

/// The file name an exported segment is offered: a map sound's name, such as
/// `ROGEAR_2.wav`, or else its bank's file and its number, such as
/// `1_1_1-12.wav`.
pub(super) fn export_file_name(bank: &Bank, segment: &Segment) -> String {
    format!("{}.wav", export_file_stem(bank, segment))
}

/// The file names a bank's segments are exported as, in its order. Map sounds
/// can share a name, so later ones are numbered, such as `Music (2).wav`.
/// Names are compared ignoring case, as macOS and Windows do.
pub(super) fn export_file_names(bank: &Bank) -> Vec<String> {
    let mut taken = HashSet::new();
    bank.segments
        .iter()
        .map(|segment| {
            let stem = export_file_stem(bank, segment);
            let mut name = stem.clone();
            let mut copy = 1;
            while !taken.insert(name.to_lowercase()) {
                copy += 1;
                name = format!("{stem} ({copy})");
            }
            format!("{name}.wav")
        })
        .collect()
}

fn export_file_stem(bank: &Bank, segment: &Segment) -> String {
    let name = segment
        .name
        .clone()
        .unwrap_or_else(|| format!("{}-{}", bank_file_stem(bank), segment.segment + 1));
    file_name_safe(&name)
}

/// The folder a bank is exported to: its file's name, such as `1_1_1`, or a
/// sound map's name and the kind of sounds, such as `MAPS.SM0 - Music`, after
/// its extra map's folder when it has one.
pub(super) fn export_folder_name(bank: &Bank) -> String {
    let file = bank_file_name(bank);
    let mut parts: Vec<&str> = bank.map.iter().map(String::as_str).collect();
    match bank.sound_kind {
        // A language's sound map has the same name as the shared one, with
        // another extension.
        Some(_) => parts.extend([file, bank.name.as_str()]),
        None => parts.push(bank_file_stem(bank)),
    }
    file_name_safe(&parts.join(" - "))
}

fn bank_file_name(bank: &Bank) -> &str {
    bank.path.rsplit('/').next().unwrap_or(&bank.path)
}

fn bank_file_stem(bank: &Bank) -> &str {
    let file = bank_file_name(bank);
    file.rsplit_once('.').map_or(file, |(stem, _)| stem)
}

/// Replaces characters some systems don't allow in file names, which map
/// sounds' names, being descriptions, may hold.
fn file_name_safe(name: &str) -> String {
    name.chars()
        .map(|c| if r#"/\:*?"<>|"#.contains(c) { '_' } else { c })
        .collect()
}

/// The bank's mission name, or its extra map's folder when `HUD` doesn't name
/// it. The main sound maps' banks, which every level shares, are grouped
/// together.
fn mission_label(bank: &Bank) -> Option<&str> {
    bank.mission_name
        .as_deref()
        .or(bank.map.as_deref())
        .or(bank.sound_kind.map(|_| "Sound maps"))
}

/// The bank's name, after its extra map's when it has one, since maps reuse
/// bank names.
pub(super) fn bank_title(bank: &Bank) -> String {
    match &bank.map {
        Some(map) => format!("{map} / {}", bank.name),
        None => bank.name.clone(),
    }
}

fn format_duration(segment: &Segment) -> String {
    let seconds = f64::from(segment.sample_count) / f64::from(segment.sample_rate.max(1));
    // Rounded to tenths before the minutes are split off, so 119.96 s is
    // 2:00.0, not 1:60.0.
    let tenths = (seconds * 10.0).round() as u64;
    if tenths < 600 {
        format!("{}.{} s", tenths / 10, tenths % 10)
    } else {
        format!("{}:{:02}.{}", tenths / 600, tenths / 10 % 60, tenths % 10)
    }
}

/// A segment action's button, with an icon before its label.
fn action_button(ui: &mut egui::Ui, icon: &str, label: &str) -> Response {
    let button =
        ui.add(egui::Button::new(format!("{icon}  {label}")).min_size(egui::vec2(96.0, 28.0)));
    // Named without its icon, which screen readers can't read.
    button.widget_info(|| WidgetInfo::labeled(Role::Button, true, label));
    button
}

/// Names an icon-only button for screen readers, which would otherwise read
/// its icon's private-use character, and shows the name as its tooltip.
fn name_icon_button(button: Response, name: &str) -> Response {
    button.widget_info(|| WidgetInfo::labeled(Role::Button, true, name));
    button.on_hover_text(name)
}

fn format_channels(channels: u32) -> String {
    match channels {
        1 => "Mono".to_owned(),
        2 => "Stereo".to_owned(),
        _ => channels.to_string(),
    }
}

fn format_key(key: Option<u32>) -> String {
    key.map_or_else(|| "—".to_owned(), |key| format!("0x{key:08X}"))
}

fn format_bytes(bytes: u64) -> String {
    const KIB: f64 = 1024.0;
    let bytes = bytes as f64;
    if bytes < KIB {
        format!("{bytes} B")
    } else if bytes < KIB * KIB {
        format!("{:.1} KB", bytes / KIB)
    } else {
        format!("{:.1} MB", bytes / (KIB * KIB))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sc_mod_tool_core::SoundKind;

    fn segment(sample_count: u32, sample_rate: u32) -> Segment {
        Segment {
            id: String::new(),
            source: SegmentSource::Stream {
                offset: 0,
                byte_length: 0,
            },
            name: None,
            sample_count,
            sample_rate,
            channels: 1,
            codec: String::new(),
            segment: 0,
        }
    }

    #[test]
    fn rounds_durations_before_splitting_off_minutes() {
        let duration = |milliseconds| format_duration(&segment(milliseconds, 1000));
        assert_eq!(duration(5_830), "5.8 s");
        assert_eq!(duration(59_940), "59.9 s");
        assert_eq!(duration(59_960), "1:00.0");
        assert_eq!(duration(119_960), "2:00.0");
        assert_eq!(duration(125_040), "2:05.0");
        assert_eq!(duration(3_599_960), "60:00.0");
    }

    fn bank(path: &str, map: Option<&str>, sound_kind: Option<SoundKind>) -> Bank {
        Bank {
            name: sound_kind.map_or_else(
                || path.rsplit('/').next().unwrap().to_owned(),
                |_| "Music".to_owned(),
            ),
            map: map.map(str::to_owned),
            sound_kind,
            path: path.to_owned(),
            code: String::new(),
            mission_name: None,
            size: 0,
            issue: None,
            segments: Vec::new(),
        }
    }

    #[test]
    fn names_export_folders_after_banks() {
        assert_eq!(
            export_folder_name(&bank("ENGLISH/1_1_1.LS0", None, None)),
            "1_1_1"
        );
        assert_eq!(
            export_folder_name(&bank("ENGLISH/MAPS.LM0", None, Some(SoundKind::Music))),
            "MAPS.LM0 - Music"
        );
        assert_eq!(
            export_folder_name(&bank(
                "1_6_1_1kolacell/MAPS.SM0",
                Some("1_6_1_1kolacell"),
                Some(SoundKind::Music)
            )),
            "1_6_1_1kolacell - MAPS.SM0 - Music"
        );
    }

    #[test]
    fn numbers_exported_sounds_that_share_a_name() {
        let mut bank = bank("MAPS.SM0", None, Some(SoundKind::Music));
        let named = |name: &str| Segment {
            name: Some(name.to_owned()),
            ..segment(1, 1)
        };
        bank.segments = vec![
            named("Sequence (multi-loop)"),
            named("ROGEAR_2"),
            named("sequence (multi-loop)"),
            named("Sequence (multi-loop)"),
            named("a/b"),
        ];
        assert_eq!(
            export_file_names(&bank),
            [
                "Sequence (multi-loop).wav",
                "ROGEAR_2.wav",
                "sequence (multi-loop) (2).wav",
                "Sequence (multi-loop) (3).wav",
                "a_b.wav",
            ]
        );
    }

    #[test]
    fn names_exported_stream_segments_by_bank_and_number() {
        let mut bank = bank("ENGLISH/1_1_1.LS0", None, None);
        bank.segments = vec![
            segment(1, 1),
            Segment {
                segment: 1,
                ..segment(1, 1)
            },
        ];
        assert_eq!(export_file_names(&bank), ["1_1_1-1.wav", "1_1_1-2.wav"]);
    }
}
