//! The selected bank's segments, in the middle.

use eframe::egui::{self, Align, CursorIcon, Layout, RichText, Sense};
use egui_extras::{Column, TableBuilder};
use egui_material_icons::icons::{
    ICON_ARROW_DOWNWARD, ICON_ARROW_UPWARD, ICON_DOWNLOAD, ICON_PLAY_ARROW, ICON_SEARCH, ICON_STOP,
    ICON_WARNING,
};
use sc_mod_tool_core::{SegmentSource, subtitles::Subtitle};

use super::banks::mission_label;
use super::widgets::name_icon_button;
use super::{Action, NO_TEXT, NO_TEXT_HINT, NOT_SHOWN_IN_GAME, is_shown_in_game};
use crate::app::format::{format_bytes, format_duration, format_key};
use crate::app::segments::table_columns;
use crate::app::{SCModToolApp, SortColumn, TextFilter};

impl SCModToolApp {
    pub(super) fn show_segments(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        let Some(bank_index) = self.selected_bank else {
            ui.centered_and_justified(|ui| {
                ui.label(RichText::new("Select a sound bank on the left.").weak());
            });
            return;
        };
        let bank = &self.banks[bank_index];

        // No space above, unlike the side panels: the central panel's frame
        // already has 6 points more margin at the top, so this starts level
        // with them.
        // The button is laid out first so it keeps its place on the right, and
        // the title and details wrap in the space left of it.
        let initial_size = egui::vec2(ui.available_width(), ui.spacing().interact_size.y);
        ui.allocate_ui_with_layout(initial_size, Layout::right_to_left(Align::Min), |ui| {
            if bank.issue.is_none() && !bank.segments.is_empty() {
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
            }
            ui.with_layout(
                Layout::left_to_right(Align::Center).with_main_wrap(true),
                |ui| {
                    ui.label(RichText::new(&bank.name).heading().strong());
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
                },
            );
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
        // The play button's column, then the segment's.
        table = table.column(Column::exact(28.0));
        for (position, &column) in columns.iter().enumerate() {
            table = table.column(column_width(column, position + 1 == columns.len()));
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
                header.col(|_| {});
                for &column in &columns {
                    header.col(|ui| {
                        let title = column_title(column);
                        let title = if sort.column == column {
                            let arrow = if sort.descending {
                                ICON_ARROW_DOWNWARD
                            } else {
                                ICON_ARROW_UPWARD
                            };
                            format!("{title} {}", arrow.codepoint)
                        } else {
                            title.to_owned()
                        };
                        if ui
                            .add(egui::Button::new(RichText::new(title).strong()).frame(false))
                            .on_hover_text("Sort by this column; click again to reverse")
                            .on_hover_cursor(CursorIcon::PointingHand)
                            .clicked()
                        {
                            actions.push(Action::SortBy(column));
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
}

/// A column's title in the segment table's header.
fn column_title(column: SortColumn) -> &'static str {
    match column {
        SortColumn::Number => "#",
        SortColumn::Duration => "Duration",
        SortColumn::Size => "Size",
        SortColumn::Offset => "Offset",
        SortColumn::UsedIn => "Used in",
        SortColumn::Entry => "Entry",
        SortColumn::Key => "Key",
        SortColumn::Name => "Name",
        SortColumn::Subtitle => "Subtitle",
    }
}

/// A column's width. The last column takes the space the others leave.
fn column_width(column: SortColumn, is_last: bool) -> Column {
    if is_last {
        return Column::remainder().clip(true);
    }
    match column {
        SortColumn::Number => Column::auto().at_least(36.0),
        SortColumn::Duration | SortColumn::Size | SortColumn::UsedIn => {
            Column::auto().at_least(64.0)
        }
        SortColumn::Entry => Column::auto().at_least(56.0),
        SortColumn::Offset | SortColumn::Key => Column::auto().at_least(80.0),
        SortColumn::Name | SortColumn::Subtitle => Column::auto().at_least(120.0).clip(true),
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
