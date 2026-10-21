//! The selected segment's details, on the right.

use eframe::egui::{self, CursorIcon, RichText};
use egui_material_icons::icons::{
    ICON_AUDIO_FILE, ICON_DOWNLOAD, ICON_HISTORY, ICON_PAUSE, ICON_PLAY_ARROW, ICON_UNDO,
};
use sc_mod_tool_core::{Area, SegmentSource, subtitles::Subtitle};

use super::replace::{file_name, fit_description};
use super::widgets::{action_button, badge, detail_row, small_detail_row};
use super::{
    Action, NO_TEXT, NO_TEXT_HINT, NOT_SHOWN_IN_GAME, NOT_SHOWN_IN_GAME_HINT, is_shown_in_game,
};
use crate::app::SCModToolApp;
use crate::app::format::{format_bytes, format_channels, format_duration, format_key};
use crate::app::names::bank_title;

impl SCModToolApp {
    pub(super) fn show_inspector(&self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
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
                            RichText::new(format!("{noun} {}", segment.segment + 1))
                                .heading()
                                .strong(),
                        );
                        ui.label(RichText::new(bank_title(bank)).weak());
                        let replacement = self.replacement(&bank.path, &segment.id);
                        if replacement.is_some() {
                            ui.add_space(4.0);
                            badge(ui, "Replaced")
                                .on_hover_text("It plays its replacement until you revert it.");
                        }
                        ui.add_space(8.0);

                        ui.horizontal_wrapped(|ui| {
                            let is_playing =
                                self.active_playback.as_ref().is_some_and(|playback| {
                                    playback.segment_id == segment.id
                                        && !playback.original
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
                            // Only numbered banks' streams can be replaced so far.
                            if segment.stream_range().is_some()
                                && action_button(ui, ICON_AUDIO_FILE.codepoint, "Replace…")
                                    .on_hover_text("Choose an audio file to play instead.")
                                    .clicked()
                            {
                                actions.push(Action::Replace(row));
                            }
                        });

                        if let Some(replacement) = replacement {
                            ui.add_space(8.0);
                            ui.separator();
                            ui.label(RichText::new("Replacement").strong());
                            egui::Grid::new("replacement")
                                .num_columns(2)
                                .spacing([16.0, 4.0])
                                .show(ui, |ui| {
                                    ui.label(RichText::new("File").weak());
                                    ui.add(egui::Label::new(file_name(&replacement.source)).wrap())
                                        .on_hover_text(replacement.source.display().to_string());
                                    ui.end_row();
                                    detail_row(
                                        ui,
                                        "Fit",
                                        fit_description(&replacement.fit, segment.sample_rate),
                                    );
                                    let saved = if self.edits.has_unsaved() {
                                        "Not saved yet"
                                    } else {
                                        "Saved to a folder, not the game"
                                    };
                                    detail_row(ui, "Status", saved);
                                });
                            ui.add_space(4.0);
                            ui.horizontal_wrapped(|ui| {
                                let original_playing =
                                    self.active_playback.as_ref().is_some_and(|playback| {
                                        playback.segment_id == segment.id
                                            && playback.original
                                            && !playback.player.is_paused()
                                    });
                                if original_playing {
                                    if action_button(ui, ICON_PAUSE.codepoint, "Pause").clicked() {
                                        actions.push(Action::TogglePause);
                                    }
                                } else if action_button(ui, ICON_HISTORY.codepoint, "Original")
                                    .on_hover_text("Play the game's own audio.")
                                    .clicked()
                                {
                                    actions.push(Action::PlayOriginal(
                                        bank.path.clone(),
                                        segment.id.clone(),
                                    ));
                                }
                                if action_button(ui, ICON_UNDO.codepoint, "Revert")
                                    .on_hover_text("Remove the replacement.")
                                    .clicked()
                                {
                                    actions.push(Action::Revert(
                                        bank.path.clone(),
                                        segment.id.clone(),
                                    ));
                                }
                            });
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
                                        detail_row(
                                            ui,
                                            "Offset",
                                            RichText::new(offset.to_string()).monospace(),
                                        );
                                    }
                                    SegmentSource::MapSound { subsong, key, .. } => {
                                        if let Some(name) = &segment.name {
                                            detail_row(ui, "Name", name.clone());
                                        }
                                        detail_row(
                                            ui,
                                            "Key",
                                            RichText::new(format_key(key)).monospace(),
                                        );
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
                            ui.horizontal(|ui| {
                                ui.label(RichText::new("Used in").strong());
                                let count = match areas.len() {
                                    1 => "1 area".to_owned(),
                                    count => format!("{count} areas"),
                                };
                                ui.label(RichText::new(count).weak());
                            });
                            egui::Grid::new("used-in")
                                .num_columns(2)
                                .spacing([16.0, 4.0])
                                .show(ui, |ui| {
                                    for (group, areas) in area_groups(areas) {
                                        detail_row(ui, group, areas.join(", "));
                                    }
                                });
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
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("Subtitles").strong());
                            if subtitles.len() > 1 {
                                ui.label(
                                    RichText::new(format!("{} entries", subtitles.len())).weak(),
                                );
                            }
                        });
                        if subtitles.is_empty() {
                            ui.label(
                                RichText::new("No subtitle is linked to this segment.").weak(),
                            );
                        }
                        ui.add_space(2.0);
                        for (position, subtitle) in subtitles.iter().enumerate() {
                            subtitle_card(ui, &segment.id, position, subtitle);
                        }
                    });
            });
    }
}

fn subtitle_card(ui: &mut egui::Ui, segment_id: &str, position: usize, subtitle: &Subtitle) {
    egui::Frame::group(ui.style()).show(ui, |ui| {
        ui.set_width(ui.available_width());
        // Read-only for now. Editing subtitles will make this a text field.
        match &subtitle.text {
            Some(text) => ui.add(egui::Label::new(text).wrap()),
            None => ui
                .label(RichText::new(NO_TEXT).italics().weak())
                .on_hover_text(NO_TEXT_HINT),
        };
        ui.add_space(2.0);
        // Keyed on the segment too, so expanding one segment's subtitle doesn't
        // expand another's. Not on the cue: scenes reuse cue names, so one
        // segment can have two lines with the same cue.
        let id = ("subtitle", segment_id, position);
        ui.horizontal(|ui| {
            ui.label(RichText::new("Cue").small().weak());
            ui.label(RichText::new(&subtitle.cue).small());
        });
        // Below the text rather than above it, so the text starts in the same
        // place in every card.
        if !is_shown_in_game(subtitle) {
            badge(ui, NOT_SHOWN_IN_GAME).on_hover_text(NOT_SHOWN_IN_GAME_HINT);
        }
        egui::CollapsingHeader::new(RichText::new("Where it comes from").small())
            .id_salt(("subtitle-source", id))
            .show(ui, |ui| {
                // Rows as tall as their small text, not a button.
                egui::Grid::new(("subtitle-source-grid", id))
                    .num_columns(2)
                    .min_row_height(0.0)
                    .spacing([12.0, 4.0])
                    .show(ui, |ui| {
                        small_detail_row(ui, "Sound", RichText::new(&subtitle.sound_event));
                        // Monospace like keys elsewhere, but small: `small()` and
                        // `monospace()` are both text styles, so only one applies.
                        small_detail_row(
                            ui,
                            "Map key",
                            RichText::new(format!("0x{:08X}", subtitle.sound_map_key))
                                .family(egui::FontFamily::Monospace),
                        );
                        ui.label(RichText::new("Script").small().weak());
                        ui.vertical(|ui| {
                            for source in &subtitle.sources {
                                ui.add(egui::Label::new(RichText::new(source).small()).wrap());
                            }
                        });
                        ui.end_row();
                    });
            })
            .header_response
            .on_hover_cursor(CursorIcon::PointingHand);
    });
    ui.add_space(6.0);
}

/// A map sound's areas by mission, like the bank list: each run of levels
/// with the same mission name, in the map's order, by their codes, then the
/// areas that aren't levels, such as `menu`, by name.
fn area_groups(areas: &[Area]) -> Vec<(&str, Vec<&str>)> {
    let mut groups: Vec<(&str, Vec<&str>)> = Vec::new();
    let mut others = Vec::new();
    for area in areas {
        let (Some(mission), Some(code)) = (&area.mission_name, &area.code) else {
            others.push(area.name.as_str());
            continue;
        };
        // Missions with the same name, such as returning to a place, stay
        // apart like they are in the bank list.
        match groups.last_mut() {
            Some((group, codes)) if group == mission => codes.push(code),
            _ => groups.push((mission, vec![code])),
        }
    }
    if !others.is_empty() {
        groups.push(("Other", others));
    }
    groups
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn groups_areas_by_mission_like_the_bank_list() {
        let area = |name: &str, code: Option<&str>, mission: Option<&str>| Area {
            name: name.to_owned(),
            code: code.map(str::to_owned),
            mission_name: mission.map(str::to_owned),
        };
        let areas = [
            area("fix", None, None),
            area(
                "4_1_1ChineseEmbassy",
                Some("4_1_1"),
                Some("Chinese Embassy"),
            ),
            area(
                "4_1_2ChineseEmbassy",
                Some("4_1_2"),
                Some("Chinese Embassy"),
            ),
            area("4_2_1_Abattoir", Some("4_2_1"), Some("Abattoir")),
            area(
                "4_3_0ChineseEmbassy",
                Some("4_3_0"),
                Some("Chinese Embassy"),
            ),
            // A level without a name in the game's localization.
            area("9_9_9Test", Some("9_9_9"), None),
            area("menu", None, None),
        ];
        assert_eq!(
            area_groups(&areas),
            [
                ("Chinese Embassy", vec!["4_1_1", "4_1_2"]),
                ("Abattoir", vec!["4_2_1"]),
                ("Chinese Embassy", vec!["4_3_0"]),
                ("Other", vec!["fix", "9_9_9Test", "menu"]),
            ]
        );
    }
}
