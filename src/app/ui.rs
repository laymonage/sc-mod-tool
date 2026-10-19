use std::time::Duration;

use eframe::egui::{self, Align, Align2, Key, Layout, Modifiers, RichText, Sense};
use egui_extras::{Column, TableBuilder};
use sc_mod_tool_core::{
    Bank, Segment, SegmentSource,
    subtitles::{MappingConfidence, Subtitle},
};

const NOT_SHOWN_IN_GAME: &str = "Not shown in game";
const NO_TEXT: &str = "No text";
const NO_TEXT_HINT: &str =
    "This line exists in its scene, but the game's localization files have no text for it.";

use super::{SCModToolApp, ScanProgress, TextFilter};
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
    StopPlayback,
    TogglePause,
    /// Move the seeker without seeking yet, or stop moving it.
    DragSeek(Option<f32>),
    Seek(Duration),
    ToggleWarnings,
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
            Action::StopPlayback => self.stop_playback(),
            Action::TogglePause => self.toggle_pause(),
            Action::DragSeek(position) => self.seek_drag = position,
            Action::Seek(position) => self.seek(position),
            Action::ToggleWarnings => self.show_warnings = !self.show_warnings,
        }
    }

    /// Arrow keys move through the segment list, and Space plays or stops the
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
            actions.push(Action::TogglePlayback(index));
        }
    }

    fn show_top_bar(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        egui::Panel::top("top-bar").show(ui, |ui| {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new("SC Mod Tool").strong().size(16.0));
                ui.separator();
                if ui.button("🗁 Open game folder…").clicked() {
                    actions.push(Action::ChooseRoot);
                }

                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if !self.languages.is_empty() {
                        let scanning = self.scan_progress.is_some();
                        if ui
                            .add_enabled(!scanning, egui::Button::new("🔄 Rescan"))
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
                            });
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
            ui.add_space(4.0);
        });
    }

    fn show_status_bar(&self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        egui::Panel::bottom("status-bar").show(ui, |ui| {
            ui.add_space(2.0);
            ui.horizontal(|ui| {
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
                            if ui.small_button("Cancel").clicked() {
                                actions.push(Action::CancelScan);
                            }
                            ui.label(RichText::new(bank).weak());
                        }
                        _ => {
                            ui.label(self.scan_activity());
                            if !self.is_cancelling_scan() && ui.small_button("Cancel").clicked() {
                                actions.push(Action::CancelScan);
                            }
                        }
                    }
                } else if self.status.is_error {
                    ui.label(
                        RichText::new(format!("⚠ {}", self.status.message))
                            .color(ui.visuals().error_fg_color),
                    );
                } else {
                    ui.label(&self.status.message);
                }

                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if !self.subtitle_warnings.is_empty()
                        && ui
                            .small_button(
                                RichText::new(format!(
                                    "⚠ {} warnings",
                                    self.subtitle_warnings.len()
                                ))
                                .color(ui.visuals().warn_fg_color),
                            )
                            .clicked()
                    {
                        actions.push(Action::ToggleWarnings);
                    }
                    if let Some(id) = self.playing_segment_id() {
                        if ui.small_button("⏹ Stop").clicked() {
                            actions.push(Action::StopPlayback);
                        }
                        let label = self.find_segment(id).map_or_else(
                            || id.to_owned(),
                            |(bank, segment)| segment_label(bank, segment),
                        );
                        let state = match &self.active_playback {
                            Some(playback) => {
                                self.show_playback_controls(ui, playback, actions);
                                if playback.sink.is_paused() {
                                    "⏸ Paused"
                                } else {
                                    "▶ Playing"
                                }
                            }
                            None => "▶ Loading",
                        };
                        ui.label(format!("{state} {label}"));
                        ui.separator();
                    }
                });
            });
            ui.add_space(2.0);
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
        let response = ui.add(egui::Slider::new(&mut position, 0.0..=total).show_value(false));
        if response.dragged() {
            actions.push(Action::DragSeek(Some(position)));
        } else if response.drag_stopped() || response.changed() {
            actions.push(Action::Seek(Duration::from_secs_f32(position)));
        }
        let pause_icon = if playback.sink.is_paused() {
            "▶"
        } else {
            "⏸"
        };
        if ui.small_button(pause_icon).clicked() {
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
                ui.add(
                    egui::TextEdit::singleline(&mut self.bank_filter)
                        .hint_text("🔍 Filter")
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
        });
        if let Some(issue) = &bank.issue {
            ui.add_space(8.0);
            egui::Frame::group(ui.style()).show(ui, |ui| {
                ui.label(
                    RichText::new("⚠ This bank can't be played")
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
                (false, _) => "🔍 Search subtitles and cues",
                (true, false) => "🔍 Search names, keys and levels",
                (true, true) => "🔍 Search names, keys, levels and subtitles",
            };
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
                    });
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
        let mut table = TableBuilder::new(ui)
            .id_salt("segments")
            .striped(true)
            .resizable(true)
            .sense(Sense::click())
            .auto_shrink([false, false])
            .cell_layout(Layout::left_to_right(Align::Center))
            .column(Column::exact(28.0))
            .column(Column::auto().at_least(36.0))
            .column(Column::auto().at_least(64.0))
            .column(Column::auto().at_least(64.0))
            .column(Column::auto().at_least(80.0));
        // Map sounds' names come before their subtitles, when they have any.
        let map_subtitles = is_map_bank && has_subtitles;
        if map_subtitles {
            table = table.column(Column::auto().at_least(120.0).clip(true));
        }
        table = table.column(Column::remainder().clip(true));
        if scroll_to_selection
            && let Some(row) = self
                .selected_segment
                .and_then(|selected| visible.iter().position(|index| *index == selected))
        {
            table = table.scroll_to_row(row, None);
        }

        let titles: &[&str] = match (is_map_bank, map_subtitles) {
            (false, _) => &["", "#", "Duration", "Size", "Offset", "Subtitle"],
            (true, false) => &["", "#", "Duration", "Used in", "Key", "Name"],
            (true, true) => &["", "#", "Duration", "Used in", "Key", "Name", "Subtitle"],
        };
        table
            .header(22.0, |mut header| {
                for &title in titles {
                    header.col(|ui| {
                        ui.strong(title);
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
                        let (icon, hint) = if is_playing {
                            ("⏹", "Stop")
                        } else {
                            ("▶", "Play")
                        };
                        if ui
                            .add(egui::Button::new(icon).frame(false))
                            .on_hover_text(hint)
                            .clicked()
                        {
                            actions.push(Action::TogglePlayback(index));
                        }
                    });
                    row.col(|ui| {
                        ui.label(format!("{}", segment.segment + 1));
                    });
                    row.col(|ui| {
                        ui.label(format_duration(segment));
                    });
                    match segment.source {
                        SegmentSource::MapSound { key, ref areas, .. } => {
                            row.col(|ui| {
                                ui.label(areas.len().to_string())
                                    .on_hover_text("How many of the map's areas play it");
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

                    let response = row.response();
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
                let (Some(bank), Some(segment)) = (self.selected_bank(), self.selected_segment())
                else {
                    ui.centered_and_justified(|ui| {
                        ui.label(RichText::new("Select a segment to see its details.").weak());
                    });
                    return;
                };
                let index = segment.segment;

                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.add_space(6.0);
                        let noun = if bank.sound_kind.is_some() {
                            "Sound"
                        } else {
                            "Segment"
                        };
                        ui.label(RichText::new(format!("{noun} {}", index + 1)).heading());
                        ui.label(RichText::new(bank_title(bank)).weak());
                        ui.add_space(8.0);

                        // Segment actions. Export and replace will go here.
                        let is_playing = self.playing_segment_id() == Some(segment.id.as_str());
                        let label = if is_playing { "⏹  Stop" } else { "▶  Play" };
                        if ui
                            .add(egui::Button::new(label).min_size(egui::vec2(96.0, 28.0)))
                            .on_hover_text("Space")
                            .clicked()
                        {
                            actions.push(Action::TogglePlayback(index));
                        }

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
        bank.segments
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
            .collect()
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
                    RichText::new("🗁  Open game folder…").size(16.0),
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
        ("⚠".to_owned(), bank.issue.as_deref())
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
            });
    });
    ui.add_space(4.0);
}

fn detail_row(ui: &mut egui::Ui, name: &str, value: String) {
    ui.label(RichText::new(name).weak());
    ui.label(value);
    ui.end_row();
}

/// Formats seconds as `m:ss`.
fn format_time(seconds: f32) -> String {
    let seconds = seconds.max(0.0) as u32;
    format!("{}:{:02}", seconds / 60, seconds % 60)
}

fn segment_label(bank: &Bank, segment: &Segment) -> String {
    format!("{} #{}", bank_title(bank), segment.segment + 1)
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
fn bank_title(bank: &Bank) -> String {
    match &bank.map {
        Some(map) => format!("{map} / {}", bank.name),
        None => bank.name.clone(),
    }
}

fn format_duration(segment: &Segment) -> String {
    let seconds = f64::from(segment.sample_count) / f64::from(segment.sample_rate.max(1));
    if seconds < 60.0 {
        format!("{seconds:.1} s")
    } else {
        format!("{}:{:04.1}", (seconds / 60.0).floor(), seconds % 60.0)
    }
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
