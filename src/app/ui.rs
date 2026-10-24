mod banks;
mod bars;
mod inspector;
mod replace;
mod segment_table;
mod widgets;

use std::time::Duration;

use eframe::egui::{self, Align2, Key, Modifiers, RichText};
use egui_material_icons::icons::ICON_FOLDER_OPEN;
use sc_mod_tool_core::subtitles::{MappingConfidence, Subtitle};

const NOT_SHOWN_IN_GAME: &str = "Not shown in game";
const NOT_SHOWN_IN_GAME_HINT: &str = "The game plays this line without a subtitle.";
const NO_TEXT: &str = "No text";
const NO_TEXT_HINT: &str =
    "This line exists in its scene, but the game's localization files have no text for it.";

use super::edits::UnsavedChoice;
use super::{SCModToolApp, SortColumn};
use sc_mod_tool_core::SoundKind;
use sc_mod_tool_core::convert::FitOptions;

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
    /// Select a segment from the playback controls and play it.
    SkipTo(usize),
    CyclePlaybackMode,
    Export(usize),
    ExportBank,
    CancelBankExport,
    TogglePause,
    /// Move the seeker without seeking yet, or stop moving it.
    DragSeek(Option<f32>),
    Seek(Duration),
    ToggleWarnings,
    SortBy(SortColumn),
    /// Choose an audio file to replace a segment with.
    Replace(usize),
    /// Play the game's own audio for a replaced segment, by bank path and
    /// segment ID.
    PlayOriginal(String, String),
    /// Remove a replacement, by bank path and segment ID.
    Revert(String, String),
    /// Remove all of a bank's replacements, by its path.
    RevertBank(String, Option<SoundKind>),
    SetDraftFit(FitOptions),
    PreviewDraft,
    ApplyDraft,
    CloseDraft,
    ChooseFfmpeg,
    Save,
    SaveToFolder,
    CancelSave,
    ResolveUnsaved(UnsavedChoice),
    /// Answer whether to save over the game's files.
    ConfirmSave(bool),
    StopPlayback,
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
        self.show_replace_dialog(&ctx, &mut actions);
        self.show_unsaved_prompt(&ctx, &mut actions);
        self.show_save_confirmation(&ctx, &mut actions);

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
            Action::ChooseRoot => self.request_choose_root(ctx),
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
            Action::SkipTo(index) => self.skip_to(ctx, index),
            Action::CyclePlaybackMode => self.cycle_playback_mode(),
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
            Action::Replace(index) => {
                self.selected_segment = Some(index);
                self.start_replacement(ctx, index);
            }
            Action::PlayOriginal(bank_path, segment_id) => {
                self.play_original(ctx, &bank_path, &segment_id);
            }
            Action::Revert(bank_path, segment_id) => self.revert_segment(&bank_path, &segment_id),
            Action::RevertBank(bank_path, kind) => self.revert_bank(&bank_path, kind),
            Action::SetDraftFit(fit) => self.set_draft_fit(fit),
            Action::PreviewDraft => self.preview_draft(ctx),
            Action::ApplyDraft => self.apply_draft(ctx),
            Action::CloseDraft => self.close_draft(),
            Action::ChooseFfmpeg => self.choose_ffmpeg(),
            Action::Save => self.request_save_in_place(ctx),
            Action::SaveToFolder => self.save_to_folder(ctx),
            Action::CancelSave => self.cancel_save(),
            Action::ResolveUnsaved(choice) => self.resolve_unsaved(ctx, choice),
            Action::ConfirmSave(confirmed) => self.confirm_save(ctx, confirmed),
            Action::StopPlayback => self.stop_playback(),
        }
    }

    /// Arrow keys move through the segment list, Space plays or pauses the
    /// selected segment, and Ctrl+S (Cmd+S on a Mac) saves.
    fn handle_keyboard(&mut self, ctx: &egui::Context, actions: &mut Vec<Action>) {
        // Dialogs have their own buttons, and the selection mustn't move
        // under them.
        if self.draft.is_some() || self.unsaved_prompt.is_some() || self.confirming_save {
            return;
        }
        if ctx.input_mut(|input| input.consume_key(Modifiers::COMMAND, Key::S)) {
            actions.push(Action::Save);
        }
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

    fn show_loading(&self, ui: &mut egui::Ui) {
        egui::CentralPanel::default().show(ui, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(ui.available_height() * 0.35);
                if self.discovering_languages || self.scan_progress.is_some() {
                    ui.spinner();
                    ui.label(self.scan_activity());
                } else {
                    ui.label(RichText::new("No sound banks to show").heading().strong());
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
            ui.label("Browse and play the game's voice and sound banks, with their subtitles,");
            ui.label("and replace voice lines with your own audio.");
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

fn is_shown_in_game(subtitle: &Subtitle) -> bool {
    subtitle.confidence == MappingConfidence::ExplicitCompiledReference
}
