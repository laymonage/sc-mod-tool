use eframe::egui::{self, Align, Align2, Key, Layout, Modifiers, RichText, Sense};
use egui_extras::{Column, TableBuilder};
use sc_mod_tool_core::{
    Bank, Segment,
    subtitles::{MappingConfidence, Subtitle},
};

const NOT_SHOWN_IN_GAME: &str = "Not shown in game";
const NO_TEXT: &str = "No text";
const NO_TEXT_HINT: &str =
    "This line exists in its scene, but the game's localization files have no text for it.";

use super::{SCModToolApp, ScanProgress, TextFilter};

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
                                    "⚠ {} subtitle warnings",
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
                        let state = if self.active_playback.is_some() {
                            "Playing"
                        } else {
                            "Loading"
                        };
                        ui.label(format!("▶ {state} {label}"));
                        ui.separator();
                    }
                });
            });
            ui.add_space(2.0);
        });
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

                let filter = self.bank_filter.to_ascii_lowercase();
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.with_layout(Layout::top_down_justified(Align::LEFT), |ui| {
                            let mut shown = 0;
                            let mut shown_map = None;
                            for (index, bank) in self.banks.iter().enumerate() {
                                if !bank.name.to_ascii_lowercase().contains(&filter)
                                    && !bank.map.as_ref().is_some_and(|map| {
                                        map.to_ascii_lowercase().contains(&filter)
                                    })
                                {
                                    continue;
                                }
                                shown += 1;
                                // Extra maps' banks come after the language's
                                // own, under the map's name.
                                if let Some(map) = &bank.map
                                    && shown_map != Some(map)
                                {
                                    ui.add_space(6.0);
                                    ui.label(RichText::new(map).small().weak());
                                    shown_map = Some(map);
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
            let map = bank
                .map
                .as_ref()
                .map(|map| format!("{map} · "))
                .unwrap_or_default();
            ui.label(
                RichText::new(format!(
                    "{map}{} segments · {}",
                    bank.segments.len(),
                    format_bytes(bank.size)
                ))
                .weak(),
            );
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
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.segment_filter)
                    .hint_text("🔍 Search subtitles and cues")
                    .desired_width(260.0),
            );
            egui::ComboBox::from_id_salt("text-filter")
                .selected_text(self.text_filter.label())
                .show_ui(ui, |ui| {
                    for filter in TextFilter::ALL {
                        ui.selectable_value(&mut self.text_filter, filter, filter.label());
                    }
                });
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
            .column(Column::auto().at_least(80.0))
            .column(Column::remainder().clip(true));
        if scroll_to_selection
            && let Some(row) = self
                .selected_segment
                .and_then(|selected| visible.iter().position(|index| *index == selected))
        {
            table = table.scroll_to_row(row, None);
        }

        table
            .header(22.0, |mut header| {
                for title in ["", "#", "Duration", "Size", "Offset", "Subtitle"] {
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
                    row.col(|ui| {
                        ui.label(format_bytes(segment.byte_length));
                    });
                    row.col(|ui| {
                        ui.label(RichText::new(segment.offset.to_string()).monospace());
                    });
                    row.col(|ui| match self.subtitles.get(&segment.id) {
                        Some(subtitles) => {
                            // Subtitles with text are sorted first, so if
                            // the first has none, none of them do.
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
                            // Subtitles the game shows are sorted next, so
                            // this means none of them are shown.
                            if is_shown_in_game(&subtitles[0]) {
                                ui.add(egui::Label::new(text).truncate());
                            } else {
                                ui.add(
                                    egui::Label::new(RichText::new(text).italics().weak())
                                        .truncate(),
                                )
                                .on_hover_text(NOT_SHOWN_IN_GAME);
                            }
                        }
                        None => {
                            ui.label(RichText::new("—").weak());
                        }
                    });

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
                        ui.label(RichText::new(format!("Segment {}", index + 1)).heading());
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
                                detail_row(
                                    ui,
                                    "Channels",
                                    if segment.channels == 1 {
                                        "Mono".to_owned()
                                    } else {
                                        segment.channels.to_string()
                                    },
                                );
                                detail_row(ui, "Format", segment.codec.to_owned());
                                detail_row(
                                    ui,
                                    "Size",
                                    format!(
                                        "{} ({} bytes)",
                                        format_bytes(segment.byte_length),
                                        segment.byte_length
                                    ),
                                );
                                detail_row(ui, "Offset", segment.offset.to_string());
                                detail_row(ui, "File", bank.path.clone());
                            });

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
        egui::Window::new("Subtitle warnings")
            .open(&mut self.show_warnings)
            .pivot(Align2::CENTER_CENTER)
            .default_pos(ctx.content_rect().center())
            .default_size([640.0, 420.0])
            .show(ctx, |ui| {
                ui.label(
                    RichText::new(
                        "Script references that could not be matched to a segment and subtitle, and sound keys that were corrected.",
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
        bank.segments
            .iter()
            .enumerate()
            .filter(|(_, segment)| {
                let subtitles = self.subtitles.get(&segment.id);
                let has_text =
                    subtitles.is_some_and(|subtitles| subtitles.iter().any(Subtitle::has_text));
                let shown = match self.text_filter {
                    TextFilter::All => true,
                    TextFilter::WithText => has_text,
                    TextFilter::WithoutText => !has_text,
                };
                shown
                    && (query.is_empty()
                        || subtitles.is_some_and(|subtitles| {
                            subtitles.iter().any(|subtitle| {
                                subtitle
                                    .text
                                    .as_deref()
                                    .is_some_and(|text| text.to_lowercase().contains(&query))
                                    || subtitle.cue.to_lowercase().contains(&query)
                            })
                        }))
            })
            .map(|(index, _)| index)
            .collect()
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

fn segment_label(bank: &Bank, segment: &Segment) -> String {
    format!("{} #{}", bank_title(bank), segment.segment + 1)
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
