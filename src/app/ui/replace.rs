//! The dialog for replacing a segment's audio, and the questions about saving
//! replacements.

use std::path::Path;

use eframe::egui::{self, Align, Layout, RichText};
use egui_material_icons::icons::{ICON_HISTORY, ICON_PLAY_ARROW, ICON_STOP};
use sc_mod_tool_core::StreamFormat;
use sc_mod_tool_core::convert::{FitMode, FitOptions, stretch_factor};

use super::Action;
use super::widgets::{action_button, detail_row};
use crate::app::SCModToolApp;
use crate::app::edits::{GuardedAction, UnsavedChoice};
use crate::app::format::{format_length, format_loop};

/// Speeds a stretched replacement can change by before it's likely to sound
/// unnatural.
const NATURAL_STRETCH: std::ops::RangeInclusive<f64> = 0.8..=1.25;

impl SCModToolApp {
    pub(super) fn show_replace_dialog(&self, ctx: &egui::Context, actions: &mut Vec<Action>) {
        let Some(draft) = &self.draft else {
            return;
        };
        let segment = &draft.segment;
        let rate = segment.sample_rate;
        let target = segment.sample_count as usize;
        let loop_start = segment
            .replaceable_stream(&draft.bank_path)
            .and_then(|stream| stream.loop_start);
        let modal = egui::Modal::new(egui::Id::unique("replace")).show(ctx, |ui| {
            ui.set_width(440.0);
            ui.label(
                RichText::new(format!("Replace segment {}", segment.segment + 1))
                    .heading()
                    .strong(),
            );
            ui.label(RichText::new(&draft.bank_path).weak());
            ui.add_space(8.0);
            egui::Grid::new("replace-details")
                .num_columns(2)
                .spacing([16.0, 4.0])
                .show(ui, |ui| {
                    ui.label(RichText::new("File").weak());
                    ui.add(egui::Label::new(file_name(&draft.source)).truncate())
                        .on_hover_text(draft.source.display().to_string());
                    ui.end_row();
                    detail_row(ui, "Segment's length", format_length(target, rate));
                    if let Some(start) = loop_start {
                        detail_row(ui, "Loops", format_loop(start, rate));
                    }
                    match &draft.samples {
                        Some(samples) => {
                            detail_row(ui, "File's length", format_length(samples.len(), rate))
                        }
                        None => {
                            ui.label(RichText::new("File's length").weak());
                            ui.horizontal(|ui| {
                                if draft.error.is_none() {
                                    ui.spinner();
                                    ui.label(RichText::new("Reading the file…").weak());
                                }
                            });
                            ui.end_row();
                        }
                    }
                });
            if loop_start.is_some() {
                ui.add_space(4.0);
                ui.add(
                    egui::Label::new(
                        RichText::new(
                            "The game repeats this sound from there to its end, so the \
                             replacement should loop cleanly from that point.",
                        )
                        .weak(),
                    )
                    .wrap(),
                );
            }

            if let Some(samples) = &draft.samples {
                ui.add_space(8.0);
                ui.separator();
                let can_change_length = segment
                    .replaceable_stream(&draft.bank_path)
                    .is_some_and(|stream| stream.format == StreamFormat::UbisoftAdpcm);
                let fit = fit_options(
                    ui,
                    &draft.fit,
                    samples.len(),
                    target,
                    rate,
                    can_change_length,
                );
                if fit != draft.fit {
                    actions.push(Action::SetDraftFit(fit));
                }
            }

            if let Some(error) = &draft.error {
                ui.add_space(8.0);
                ui.add(
                    egui::Label::new(RichText::new(error).color(ui.visuals().error_fg_color))
                        .wrap(),
                );
                if error.contains("FFmpeg") && ui.button("Choose FFmpeg…").clicked() {
                    actions.push(Action::ChooseFfmpeg);
                }
            }

            ui.add_space(12.0);
            let ready = draft.samples.is_some() && !draft.is_busy();
            ui.horizontal(|ui| {
                if ui
                    .add_enabled_ui(ready, |ui| {
                        action_button(ui, ICON_PLAY_ARROW.codepoint, "Preview")
                    })
                    .inner
                    .on_hover_text("Play the replacement as it would be saved.")
                    .clicked()
                {
                    actions.push(Action::PreviewDraft);
                }
                if action_button(ui, ICON_HISTORY.codepoint, "Original")
                    .on_hover_text("Play the segment's own audio.")
                    .clicked()
                {
                    actions.push(Action::PlayOriginal(
                        draft.bank_path.clone(),
                        segment.id.clone(),
                    ));
                }
                let playing = self.playing_segment_id() == Some(segment.id.as_str());
                if ui
                    .add_enabled_ui(playing, |ui| action_button(ui, ICON_STOP.codepoint, "Stop"))
                    .inner
                    .clicked()
                {
                    actions.push(Action::StopPlayback);
                }
                if draft.is_busy() && draft.samples.is_some() {
                    ui.spinner();
                }
            });
            ui.add_space(8.0);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui
                    .add_enabled(ready, egui::Button::new("Replace"))
                    .on_hover_text("Keep the replacement until you save.")
                    .clicked()
                {
                    actions.push(Action::ApplyDraft);
                }
                if ui.button("Cancel").clicked() {
                    actions.push(Action::CloseDraft);
                }
            });
        });
        if modal.should_close() {
            actions.push(Action::CloseDraft);
        }
    }

    pub(super) fn show_unsaved_prompt(&self, ctx: &egui::Context, actions: &mut Vec<Action>) {
        let Some(action) = &self.unsaved_prompt else {
            return;
        };
        let count = self.edits.len();
        let segments = if count == 1 { "segment" } else { "segments" };
        let before = match action {
            GuardedAction::OpenRoot(_) => "opening another game folder",
            GuardedAction::Quit => "quitting",
        };
        let maps = if self.edits.changes_lengths() {
            " Replacements of another length change the sound maps beside them too."
        } else {
            ""
        };
        let modal = egui::Modal::new(egui::Id::unique("unsaved")).show(ctx, |ui| {
            ui.set_width(400.0);
            ui.label(RichText::new("Save your replacements?").heading().strong());
            ui.add_space(6.0);
            ui.add(
                egui::Label::new(format!(
                    "{count} replaced {segments} haven't been saved. Save them over the game's \
                     files before {before}?{maps} The game's own files are kept beside them, \
                     ending in .orig."
                ))
                .wrap(),
            );
            ui.add_space(12.0);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.button("Save").clicked() {
                    actions.push(Action::ResolveUnsaved(UnsavedChoice::Save));
                }
                if ui.button("Don't save").clicked() {
                    actions.push(Action::ResolveUnsaved(UnsavedChoice::Discard));
                }
                if ui.button("Cancel").clicked() {
                    actions.push(Action::ResolveUnsaved(UnsavedChoice::Cancel));
                }
            });
        });
        if modal.should_close() {
            actions.push(Action::ResolveUnsaved(UnsavedChoice::Cancel));
        }
    }

    pub(super) fn show_save_confirmation(&self, ctx: &egui::Context, actions: &mut Vec<Action>) {
        if !self.confirming_save {
            return;
        }
        let count = self.edits.len();
        let segments = if count == 1 { "segment" } else { "segments" };
        let files = match self.edits.file_count() {
            1 => "1 file".to_owned(),
            files => format!("{files} files"),
        };
        let maps = if self.edits.changes_lengths() {
            ", and changes the sound map beside any of another length"
        } else {
            ""
        };
        let modal = egui::Modal::new(egui::Id::unique("confirm-save")).show(ctx, |ui| {
            ui.set_width(400.0);
            ui.label(
                RichText::new("Save over the game's files?")
                    .heading()
                    .strong(),
            );
            ui.add_space(6.0);
            ui.add(
                egui::Label::new(format!(
                    "This writes {count} replaced {segments} into {files} in the game's Sounds \
                     folder{maps}. Before a file is first changed, the game's own file is \
                     copied beside it, ending in .orig, so it can be put back."
                ))
                .wrap(),
            );
            ui.add_space(12.0);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.button("Save").clicked() {
                    actions.push(Action::ConfirmSave(true));
                }
                if ui.button("Cancel").clicked() {
                    actions.push(Action::ConfirmSave(false));
                }
            });
        });
        if modal.should_close() {
            actions.push(Action::ConfirmSave(false));
        }
    }
}

/// The controls for which part of a file of `length` samples to use and how
/// to fit it to `target` samples, returning the options as changed. Only a
/// segment that `can_change_length` offers to keep the file's own length.
fn fit_options(
    ui: &mut egui::Ui,
    current: &FitOptions,
    length: usize,
    target: usize,
    rate: u32,
    can_change_length: bool,
) -> FitOptions {
    let rate_f = f64::from(rate.max(1));
    let mut fit = current.clone();
    let end = fit.end.unwrap_or(length).min(length);
    let mut start_seconds = fit.start.min(end) as f64 / rate_f;
    let mut end_seconds = end as f64 / rate_f;
    let length_seconds = length as f64 / rate_f;

    ui.label(RichText::new("Part of the file").strong());
    ui.horizontal(|ui| {
        ui.label("From");
        ui.add(
            egui::DragValue::new(&mut start_seconds)
                .range(0.0..=end_seconds)
                .speed(0.01)
                .max_decimals(2)
                .suffix(" s"),
        );
        ui.label("to");
        ui.add(
            egui::DragValue::new(&mut end_seconds)
                .range(start_seconds..=length_seconds)
                .speed(0.01)
                .max_decimals(2)
                .suffix(" s"),
        );
    });
    fit.start = (start_seconds * rate_f).round() as usize;
    let end = ((end_seconds * rate_f).round() as usize).min(length);
    fit.end = (end < length).then_some(end);
    let selected = end.saturating_sub(fit.start);
    ui.label(RichText::new(format!("{} chosen", format_length(selected, rate))).weak());

    ui.add_space(6.0);
    ui.label(RichText::new("Fit it to the segment").strong());
    ui.radio_value(&mut fit.mode, FitMode::TrimPad, "Trim or pad with silence");
    if fit.mode == FitMode::TrimPad && selected != target {
        let hint = if selected > target {
            "The end is cut off."
        } else {
            "Silence is added at the end."
        };
        ui.label(RichText::new(hint).weak());
    }
    let factor = stretch_factor(selected.max(1), target);
    let speed = if factor >= 1.0 {
        format!("{factor:.2}× faster")
    } else {
        format!("{factor:.2}× speed")
    };
    ui.radio_value(
        &mut fit.mode,
        FitMode::Stretch,
        format!("Change its speed, keeping its pitch ({speed})"),
    );
    if fit.mode == FitMode::Stretch && !NATURAL_STRETCH.contains(&factor) {
        ui.label(
            RichText::new("Changing the speed this much may sound unnatural.")
                .color(ui.visuals().warn_fg_color),
        );
    }
    let keep_length = ui
        .add_enabled(
            can_change_length,
            egui::RadioButton::new(
                fit.mode == FitMode::KeepLength,
                "Keep its own length (experimental)",
            ),
        )
        .on_hover_text(
            "The segments after it in the bank move, and the sound map, MAPS.LM0, is changed \
             to match.",
        )
        .on_disabled_hover_text(
            "A sound stored in a sound map has to keep its length, as another would move \
             every sound after it.",
        );
    if keep_length.clicked() {
        fit.mode = FitMode::KeepLength;
    }
    if fit.mode == FitMode::KeepLength && selected != target {
        ui.add(
            egui::Label::new(
                RichText::new(
                    "Saving changes the sound map, MAPS.LM0, too. Its subtitle shows for the new \
                     length, but lip-sync stays timed to the old one.",
                )
                .color(ui.visuals().warn_fg_color),
            )
            .wrap(),
        );
    }
    fit
}

/// How a replacement was fitted to its segment, for the inspector.
pub(super) fn fit_description(fit: &FitOptions, rate: u32) -> String {
    let mode = match fit.mode {
        FitMode::TrimPad => "Trimmed or padded",
        FitMode::Stretch => "Speed changed",
        FitMode::KeepLength => "Its own length",
    };
    let seconds = |samples: usize| samples as f64 / f64::from(rate.max(1));
    match (fit.start, fit.end) {
        (0, None) => mode.to_owned(),
        (start, None) => format!("{mode}, from {:.2} s", seconds(start)),
        (start, Some(end)) => format!(
            "{mode}, from {:.2} s to {:.2} s",
            seconds(start),
            seconds(end)
        ),
    }
}

pub(super) fn file_name(path: &Path) -> String {
    path.file_name()
        .unwrap_or(path.as_os_str())
        .to_string_lossy()
        .into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::edits::ReplacementDraft;
    use crate::app::test_support::{bank, stream_segment};
    use egui_kittest::Harness;
    use egui_kittest::kittest::{NodeT, Queryable};
    use egui_material_icons::icons::ICON_SAVE;

    use eframe::egui::accesskit::Role;

    const BANK: &str = "ENGLISH/1_1_0.LS0";

    /// The app with a voice bank of two segments, the first selected, and a
    /// replacement for it ready in the dialog.
    fn harness() -> Harness<'static, SCModToolApp> {
        Harness::builder()
            .with_size([1200.0, 760.0])
            .build_eframe(|cc| {
                crate::app::setup_context(&cc.egui_ctx);
                let mut app = SCModToolApp::new(None);
                app.root = Some("game".into());
                let mut voices = bank(BANK, None, None);
                voices.segments = vec![stream_segment(BANK, 0, 10), stream_segment(BANK, 200, 20)];
                voices.segments[1].segment = 1;
                app.banks = vec![voices];
                app.selected_bank = Some(0);
                app.selected_segment = Some(0);
                app.draft = Some(ReplacementDraft::prepared(
                    BANK,
                    stream_segment(BANK, 0, 10),
                    10,
                ));
                app
            })
    }

    #[test]
    fn replacing_through_the_dialog() {
        let mut harness = harness();
        harness.run();
        harness
            .get_by_role_and_label(Role::Button, "Replace")
            .click();
        harness.run();
        let app = harness.state();
        assert!(app.draft.is_none());
        assert_eq!(app.edits.bank_len(BANK, None), 1);

        // The inspector offers to revert it.
        harness
            .get_by_role_and_label(Role::Button, "Revert")
            .click();
        harness.run();
        assert!(harness.state().edits.is_empty());
    }

    #[test]
    fn a_replacement_can_keep_its_own_length() {
        let mut harness = harness();
        harness.run();
        harness
            .get_by_role_and_label(Role::RadioButton, "Keep its own length (experimental)")
            .click();
        harness.run();
        let draft = harness.state().draft.as_ref().unwrap();
        assert_eq!(draft.fit.mode, FitMode::KeepLength);
        // The file is as long as the segment, so nothing else changes.
        harness.get_by_role_and_label(Role::Button, "Replace");
    }

    #[test]
    fn a_sound_stored_in_a_map_keeps_its_length() {
        let mut harness = harness();
        {
            let draft = harness.state_mut().draft.as_mut().unwrap();
            draft.bank_path = "MAPS.SM0".to_owned();
            draft.segment.source = sc_mod_tool_core::SegmentSource::MapSound {
                subsong: 1,
                key: Some(1),
                areas: Vec::new(),
                stream: Some(sc_mod_tool_core::FileStream {
                    path: "MAPS.SM0".to_owned(),
                    offsets: vec![64, 128],
                    byte_length: 20,
                    format: StreamFormat::Pcm16,
                    loop_start: None,
                }),
                pitch: sc_mod_tool_core::NORMAL_PITCH,
            };
        }
        harness.run();
        let keep =
            harness.get_by_role_and_label(Role::RadioButton, "Keep its own length (experimental)");
        assert!(keep.accesskit_node().is_disabled());
        keep.click();
        harness.run();
        assert_eq!(
            harness.state().draft.as_ref().unwrap().fit.mode,
            FitMode::TrimPad
        );
    }

    #[test]
    fn says_where_a_looping_sound_loops_from() {
        let mut harness = harness();
        {
            let draft = harness.state_mut().draft.as_mut().unwrap();
            draft.bank_path = "MAPS.SM0".to_owned();
            draft.segment.source = sc_mod_tool_core::SegmentSource::MapSound {
                subsong: 1,
                key: Some(1),
                areas: Vec::new(),
                stream: Some(sc_mod_tool_core::FileStream {
                    path: "MAPS.SM0".to_owned(),
                    offsets: vec![64],
                    byte_length: 20,
                    format: StreamFormat::Pcm16,
                    loop_start: Some(0),
                }),
                pitch: sc_mod_tool_core::NORMAL_PITCH,
            };
        }
        harness.run();
        harness.get_by_label("From its start");
        harness.get_by_label_contains("should loop cleanly");
    }

    #[test]
    fn saving_asks_first() {
        let mut harness = harness();
        harness.run();
        harness
            .get_by_role_and_label(Role::Button, "Replace")
            .click();
        harness.run();

        let save = format!("{} Save", ICON_SAVE.codepoint);
        harness.get_by_role_and_label(Role::Button, &save).click();
        harness.run();
        assert!(harness.state().confirming_save);
        harness
            .get_by_role_and_label(Role::Button, "Cancel")
            .click();
        harness.run();
        let app = harness.state();
        assert!(!app.confirming_save);
        assert!(!app.is_saving());
        assert_eq!(app.edits.len(), 1);
    }

    #[test]
    fn quitting_asks_about_unsaved_replacements() {
        let mut harness = harness();
        harness.run();
        harness
            .get_by_role_and_label(Role::Button, "Replace")
            .click();
        harness.run();
        harness.state_mut().unsaved_prompt = Some(GuardedAction::Quit);
        harness.run();
        harness.get_by_label_contains("1 replaced segment haven't");
        harness
            .get_by_role_and_label(Role::Button, "Don't save")
            .click();
        harness.run();
        let app = harness.state();
        assert!(app.edits.is_empty());
        assert!(app.closing);
    }

    #[test]
    fn describes_how_replacements_were_fitted() {
        let fit = |mode, start, end| FitOptions { mode, start, end };
        assert_eq!(
            fit_description(&fit(FitMode::TrimPad, 0, None), 36000),
            "Trimmed or padded"
        );
        assert_eq!(
            fit_description(&fit(FitMode::Stretch, 18000, Some(54000)), 36000),
            "Speed changed, from 0.50 s to 1.50 s"
        );
        assert_eq!(
            fit_description(&fit(FitMode::TrimPad, 36000, None), 36000),
            "Trimmed or padded, from 1.00 s"
        );
    }
}
