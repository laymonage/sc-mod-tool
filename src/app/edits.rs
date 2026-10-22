//! Replacing segments: the replacements made but not saved yet, preparing one
//! from an audio file, and saving them over the game's files or into a folder.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;

use eframe::egui;
use sc_mod_tool_core::convert::{self, Ffmpeg, FitOptions};
use sc_mod_tool_core::decoder::DecodedAudio;
use sc_mod_tool_core::edit::{self, BankEdit, SaveReport, SaveTarget};
use sc_mod_tool_core::{Error, Segment, SoundKind, StreamFormat, decoder};

use super::{SCModToolApp, WorkerMessage};
use crate::audio::PlayableAudio;

/// The audio files offered when choosing a replacement. FFmpeg reads many
/// more, which "All files" allows.
const AUDIO_EXTENSIONS: [&str; 9] = [
    "wav", "mp3", "ogg", "flac", "m4a", "aac", "opus", "wma", "aiff",
];

/// A segment's new audio, encoded and ready to save.
#[derive(Clone)]
pub(super) struct Replacement {
    /// The audio file it was made from.
    pub(super) source: PathBuf,
    pub(super) fit: FitOptions,
    /// The file holding the stream it replaces, relative to `Sounds`: its
    /// bank, or for a map's sound, the file the map points at or the map.
    pub(super) file: String,
    /// Where each copy of the stream it replaces starts in its file.
    pub(super) offsets: Vec<u64>,
    /// The kind of the bank it was made in, which tells apart a map's banks
    /// of different kinds, as they share the map's path.
    pub(super) bank_kind: Option<SoundKind>,
    pub(super) stream: Arc<[u8]>,
    pub(super) sample_count: u32,
    /// Whether it's another length than the stream it replaces, so saving it
    /// moves the streams after it and changes the sound map.
    pub(super) changes_length: bool,
    /// The stream decoded again, so what plays is what's saved.
    pub(super) preview: PlayableAudio,
}

/// Replacements that aren't in the game's files yet, by bank and segment. They
/// don't depend on which banks are loaded, so they're kept when switching
/// languages.
#[derive(Default)]
pub(super) struct PendingEdits {
    banks: BTreeMap<String, BTreeMap<String, Replacement>>,
    /// Whether they changed since they were last saved or exported.
    unsaved: bool,
}

impl PendingEdits {
    pub(super) fn get(&self, bank_path: &str, segment_id: &str) -> Option<&Replacement> {
        self.banks.get(bank_path)?.get(segment_id)
    }

    pub(super) fn insert(&mut self, bank_path: &str, segment_id: &str, replacement: Replacement) {
        self.banks
            .entry(bank_path.to_owned())
            .or_default()
            .insert(segment_id.to_owned(), replacement);
        self.unsaved = true;
    }

    pub(super) fn remove(&mut self, bank_path: &str, segment_id: &str) {
        let Some(bank) = self.banks.get_mut(bank_path) else {
            return;
        };
        if bank.remove(segment_id).is_some() {
            self.unsaved = true;
        }
        if bank.is_empty() {
            self.banks.remove(bank_path);
        }
    }

    /// Removes the replacements made in the bank at `bank_path` of `kind`.
    pub(super) fn remove_bank(&mut self, bank_path: &str, kind: Option<SoundKind>) {
        let Some(bank) = self.banks.get_mut(bank_path) else {
            return;
        };
        let before = bank.len();
        bank.retain(|_, replacement| replacement.bank_kind != kind);
        if bank.len() != before {
            self.unsaved = true;
        }
        if bank.is_empty() {
            self.banks.remove(bank_path);
        }
    }

    pub(super) fn clear(&mut self) {
        self.banks.clear();
        self.unsaved = false;
    }

    /// How many segments are replaced.
    pub(super) fn len(&self) -> usize {
        self.banks.values().map(BTreeMap::len).sum()
    }

    pub(super) fn is_empty(&self) -> bool {
        self.banks.is_empty()
    }

    /// Whether any replacement is another length than the stream it
    /// replaces.
    pub(super) fn changes_lengths(&self) -> bool {
        self.banks
            .values()
            .flat_map(BTreeMap::values)
            .any(|replacement| replacement.changes_length)
    }

    /// How many files the replacements go into: banks, or for a map's
    /// sounds, the files the map points at.
    pub(super) fn file_count(&self) -> usize {
        let files: BTreeSet<_> = self
            .banks
            .values()
            .flat_map(BTreeMap::values)
            .map(|replacement| &replacement.file)
            .collect();
        files.len()
    }

    /// How many of the bank's segments are replaced.
    pub(super) fn bank_len(&self, bank_path: &str, kind: Option<SoundKind>) -> usize {
        self.banks.get(bank_path).map_or(0, |bank| {
            bank.values()
                .filter(|replacement| replacement.bank_kind == kind)
                .count()
        })
    }

    /// Whether there are replacements that haven't been saved or exported
    /// since they were made or reverted. Reverting the last one leaves
    /// nothing to save.
    pub(super) fn has_unsaved(&self) -> bool {
        self.unsaved && !self.banks.is_empty()
    }

    /// The replacements, by the file each goes into.
    fn bank_edits(&self) -> Vec<BankEdit> {
        let mut files = BTreeMap::<&str, Vec<edit::Replacement>>::new();
        for replacement in self.banks.values().flat_map(BTreeMap::values) {
            let file = files.entry(&replacement.file).or_default();
            for &offset in &replacement.offsets {
                file.push(edit::Replacement {
                    offset,
                    stream: replacement.stream.to_vec(),
                });
            }
        }
        files
            .into_iter()
            .map(|(path, replacements)| BankEdit {
                path: path.to_owned(),
                replacements,
            })
            .collect()
    }

    /// Forgets the replacements in the files `saved` over the game's, which
    /// now hold them.
    fn saved_in_place(&mut self, saved: &[String]) {
        for replacements in self.banks.values_mut() {
            replacements.retain(|_, replacement| !saved.contains(&replacement.file));
        }
        self.banks
            .retain(|_, replacements| !replacements.is_empty());
        self.unsaved = !self.banks.is_empty();
    }

    /// Notes that every replacement was exported. They're kept, so that the
    /// next export has them too.
    fn exported(&mut self) {
        self.unsaved = false;
    }
}

/// A replacement being made for a segment, before it's applied.
pub(super) struct ReplacementDraft {
    pub(super) bank_path: String,
    /// The kind of the bank, if it's a map's.
    bank_kind: Option<SoundKind>,
    pub(super) segment: Segment,
    /// The audio file chosen.
    pub(super) source: PathBuf,
    /// The file's audio at the segment's sample rate, once read.
    pub(super) samples: Option<Arc<Vec<i16>>>,
    /// How the audio is made to fit, which the user can change.
    pub(super) fit: FitOptions,
    /// The replacement for `fit`, once prepared.
    pub(super) prepared: Option<Replacement>,
    pub(super) error: Option<String>,
    job: Option<DraftJob>,
}

impl ReplacementDraft {
    /// Whether the file is being read or a replacement prepared.
    pub(super) fn is_busy(&self) -> bool {
        self.job.is_some()
    }

    /// The replacement for the current options, if it's been prepared.
    pub(super) fn current(&self) -> Option<&Replacement> {
        self.prepared
            .as_ref()
            .filter(|prepared| prepared.fit == self.fit)
    }
}

#[cfg(test)]
impl ReplacementDraft {
    /// A draft for `segment`, with a replacement of `sample_count` samples
    /// already prepared for its options.
    pub(super) fn prepared(bank_path: &str, segment: Segment, sample_count: u32) -> Self {
        let stream = segment.replaceable_stream(bank_path).unwrap();
        let mut prepared =
            super::test_support::replacement(stream.path, stream.offsets[0], sample_count);
        prepared.changes_length = sample_count != segment.sample_count;
        Self {
            bank_path: bank_path.to_owned(),
            bank_kind: None,
            segment,
            source: PathBuf::from("line.wav"),
            samples: Some(Arc::new(vec![0; sample_count as usize])),
            fit: FitOptions::default(),
            prepared: Some(prepared),
            error: None,
            job: None,
        }
    }
}

/// Work on a draft in a worker thread.
struct DraftJob {
    request: u64,
    cancel: Arc<AtomicBool>,
    then: AfterPrepare,
}

/// What to do once a draft's replacement is prepared.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum AfterPrepare {
    /// Only read the file, to show how long it is.
    Nothing,
    Play,
    Apply,
}

/// A save in progress. Only one runs at a time.
pub(super) struct SaveJob {
    pub(super) target: SaveTarget,
    pub(super) cancel: Arc<AtomicBool>,
    /// How many files the replacements go into.
    files: usize,
    /// What to do once everything is saved, after asking about unsaved
    /// replacements.
    then: Option<GuardedAction>,
}

/// Something that would lose unsaved replacements, so it's asked about first.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum GuardedAction {
    /// Open the game folder chosen.
    OpenRoot(PathBuf),
    Quit,
}

impl GuardedAction {
    /// What it is, to say it didn't happen.
    fn description(&self) -> &'static str {
        match self {
            GuardedAction::OpenRoot(_) => "The other game folder wasn't opened.",
            GuardedAction::Quit => "The app didn't quit.",
        }
    }
}

/// The answers to the question about unsaved replacements.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum UnsavedChoice {
    Save,
    Discard,
    Cancel,
}

impl SCModToolApp {
    /// The replacement for the segment, if it has one.
    pub(super) fn replacement(&self, bank_path: &str, segment_id: &str) -> Option<&Replacement> {
        self.edits.get(bank_path, segment_id)
    }

    /// Asks for an audio file, then starts reading it to replace the
    /// segment.
    pub(super) fn start_replacement(&mut self, ctx: &egui::Context, segment_index: usize) {
        let Some(bank) = self.selected_bank() else {
            return;
        };
        let Some(segment) = bank.segments.get(segment_index).cloned() else {
            return;
        };
        if segment.replaceable_stream(&bank.path).is_none() {
            self.set_status(edit::CANT_REPLACE, true);
            return;
        }
        let bank_path = bank.path.clone();
        let mut dialog = rfd::FileDialog::new()
            .set_title("Choose the replacement audio")
            .add_filter("Audio", &AUDIO_EXTENSIONS)
            .add_filter("All files", &["*"]);
        if let Some(directory) = &self.source_directory {
            dialog = dialog.set_directory(directory);
        }
        let Some(source) = dialog.pick_file() else {
            return;
        };
        self.source_directory = source.parent().map(Path::to_owned);
        self.open_draft(ctx, bank_path, segment, source);
    }

    /// Starts a draft replacing `segment` with `source`, reading the file.
    fn open_draft(
        &mut self,
        ctx: &egui::Context,
        bank_path: String,
        segment: Segment,
        source: PathBuf,
    ) {
        self.close_draft();
        let bank_kind = self
            .banks
            .iter()
            .find(|bank| {
                bank.path == bank_path && bank.segments.iter().any(|other| other.id == segment.id)
            })
            .and_then(|bank| bank.sound_kind);
        self.draft = Some(ReplacementDraft {
            bank_path,
            bank_kind,
            segment,
            source,
            samples: None,
            fit: FitOptions::default(),
            prepared: None,
            error: None,
            job: None,
        });
        self.start_draft_job(ctx, AfterPrepare::Nothing);
    }

    /// Changes how the draft's audio is fitted. A replacement being prepared
    /// for the old options is no longer wanted, so that work stops.
    pub(super) fn set_draft_fit(&mut self, fit: FitOptions) {
        let Some(draft) = &mut self.draft else {
            return;
        };
        draft.fit = fit;
        draft.error = None;
        // Unless the file is still being read, which the new options need too.
        if draft.samples.is_some()
            && let Some(job) = draft.job.take()
        {
            job.cancel.store(true, Ordering::Relaxed);
        }
    }

    /// Plays the draft's replacement, preparing it first if needed.
    pub(super) fn preview_draft(&mut self, ctx: &egui::Context) {
        let Some(draft) = &self.draft else {
            return;
        };
        match draft.current() {
            Some(prepared) => {
                let preview = prepared.preview.clone();
                let segment_id = draft.segment.id.clone();
                self.play_preview(preview, segment_id);
            }
            None => self.start_draft_job(ctx, AfterPrepare::Play),
        }
    }

    /// Adds the draft's replacement to the unsaved ones, preparing it first if
    /// needed, and closes the draft.
    pub(super) fn apply_draft(&mut self, ctx: &egui::Context) {
        let Some(draft) = &self.draft else {
            return;
        };
        if draft.current().is_some() {
            self.finish_draft();
        } else {
            self.start_draft_job(ctx, AfterPrepare::Apply);
        }
    }

    /// Closes the draft, stopping any work on it.
    pub(super) fn close_draft(&mut self) {
        if let Some(job) = self.draft.take().and_then(|draft| draft.job) {
            job.cancel.store(true, Ordering::Relaxed);
        }
    }

    fn finish_draft(&mut self) {
        let Some(draft) = self.draft.take() else {
            return;
        };
        let Some(mut replacement) = draft.current().cloned() else {
            self.draft = Some(draft);
            return;
        };
        replacement.bank_kind = draft.bank_kind;
        self.forget_playback_of(&draft.segment.id);
        self.edits
            .insert(&draft.bank_path, &draft.segment.id, replacement);
        self.set_status(
            format!(
                "Replaced {}. Save to write it to the game's files.",
                draft.segment.id
            ),
            false,
        );
    }

    /// Reads the draft's file if it hasn't been, then, unless `then` is
    /// [`AfterPrepare::Nothing`], prepares the replacement for its options.
    fn start_draft_job(&mut self, ctx: &egui::Context, then: AfterPrepare) {
        let Some(root) = self.root.clone() else {
            return;
        };
        let configured = self.ffmpeg_path.clone();
        self.draft_request = self.draft_request.wrapping_add(1);
        let request = self.draft_request;
        let Some(draft) = &mut self.draft else {
            return;
        };
        if let Some(job) = draft.job.take() {
            job.cancel.store(true, Ordering::Relaxed);
        }
        let cancel = Arc::new(AtomicBool::new(false));
        draft.job = Some(DraftJob {
            request,
            cancel: Arc::clone(&cancel),
            then,
        });
        draft.error = None;
        let bank_path = draft.bank_path.clone();
        let segment = draft.segment.clone();
        let source = draft.source.clone();
        let samples = draft.samples.clone();
        let fit = draft.fit.clone();
        let sender = self.sender.clone();
        let repaint = ctx.clone();

        thread::spawn(move || {
            let result = Ffmpeg::locate(configured.as_deref()).and_then(|ffmpeg| {
                let samples = match samples {
                    Some(samples) => samples,
                    None => Arc::new(ffmpeg.decode(&source, segment.sample_rate, &cancel)?),
                };
                let prepared = match then {
                    AfterPrepare::Nothing => None,
                    AfterPrepare::Play | AfterPrepare::Apply => Some(prepare(
                        &root, &bank_path, &segment, &ffmpeg, &source, &samples, fit, &cancel,
                    )?),
                };
                Ok((samples, prepared))
            });
            let _ = sender.send(WorkerMessage::Draft { request, result });
            repaint.request_repaint();
        });
    }

    pub(super) fn finish_draft_job(
        &mut self,
        request: u64,
        result: Result<(Arc<Vec<i16>>, Option<Replacement>), Error>,
    ) {
        let Some(draft) = &mut self.draft else {
            return;
        };
        if draft.job.as_ref().is_none_or(|job| job.request != request) {
            return;
        }
        let then = draft
            .job
            .take()
            .map_or(AfterPrepare::Nothing, |job| job.then);
        match result {
            Ok((samples, prepared)) => {
                draft.samples = Some(samples);
                if prepared.is_some() {
                    draft.prepared = prepared;
                }
            }
            Err(Error::Cancelled) => return,
            Err(error) => {
                draft.error = Some(error.to_string());
                return;
            }
        }
        match then {
            AfterPrepare::Nothing => {}
            AfterPrepare::Play => {
                if let Some(prepared) = draft.current() {
                    let preview = prepared.preview.clone();
                    let segment_id = draft.segment.id.clone();
                    self.play_preview(preview, segment_id);
                }
            }
            AfterPrepare::Apply => self.finish_draft(),
        }
    }

    /// Removes the segment's replacement.
    pub(super) fn revert_segment(&mut self, bank_path: &str, segment_id: &str) {
        self.forget_playback_of(segment_id);
        self.edits.remove(bank_path, segment_id);
    }

    /// Removes all the replacements in the bank at `bank_path` of `kind`.
    pub(super) fn revert_bank(&mut self, bank_path: &str, kind: Option<SoundKind>) {
        if self.loaded_segment_id().is_some_and(|id| {
            self.edits
                .get(bank_path, id)
                .is_some_and(|replacement| replacement.bank_kind == kind)
        }) {
            self.clear_playback();
        }
        self.edits.remove_bank(bank_path, kind);
    }

    /// Stops playing the segment if it's loaded, as what it plays is changing.
    fn forget_playback_of(&mut self, segment_id: &str) {
        if self.loaded_segment_id() == Some(segment_id) {
            self.clear_playback();
        }
    }

    pub(super) fn is_saving(&self) -> bool {
        self.save_job.is_some()
    }

    /// Writes the replacements over the game's files, asking first the first
    /// time, and again the first time it would change a sound map.
    pub(super) fn request_save_in_place(&mut self, ctx: &egui::Context) {
        if self.save_confirmed && (self.map_change_confirmed || !self.edits.changes_lengths()) {
            self.save_in_place(ctx);
        } else {
            self.confirming_save = true;
        }
    }

    pub(super) fn confirm_save(&mut self, ctx: &egui::Context, confirmed: bool) {
        self.confirming_save = false;
        if confirmed {
            self.save_confirmed = true;
            self.map_change_confirmed |= self.edits.changes_lengths();
            self.save_in_place(ctx);
        }
    }

    /// Writes the replacements over the game's files.
    pub(super) fn save_in_place(&mut self, ctx: &egui::Context) {
        self.start_save(ctx, SaveTarget::InPlace, None);
    }

    /// Asks for a folder, then writes the replaced banks into it.
    pub(super) fn save_to_folder(&mut self, ctx: &egui::Context) {
        let mut dialog = rfd::FileDialog::new().set_title("Save the replaced banks to a folder");
        if let Some(directory) = &self.export_directory {
            dialog = dialog.set_directory(directory);
        }
        let Some(folder) = dialog.pick_folder() else {
            return;
        };
        self.export_directory = Some(folder.clone());
        self.start_save(ctx, SaveTarget::Folder(folder), None);
    }

    /// Starts saving to `target`, then doing `then` if everything is saved.
    /// Without anything to save, `then` is done straight away.
    fn start_save(&mut self, ctx: &egui::Context, target: SaveTarget, then: Option<GuardedAction>) {
        if self.edits.is_empty() {
            match then {
                Some(action) => self.perform(ctx, action),
                None => self.set_status("There's nothing to save.", false),
            }
            return;
        }
        let refusal = if self.save_job.is_some() {
            Some("Wait for the save to finish.")
        } else if self.scan_cancel.is_some() || self.discovering_languages {
            // A scan may have the files open.
            Some("Wait for the scan to finish before saving.")
        } else if self.root.is_none() {
            Some("Open the game folder first.")
        } else {
            None
        };
        let Some(root) = self.root.clone().filter(|_| refusal.is_none()) else {
            let refusal = refusal.unwrap_or_default();
            let message = match &then {
                Some(action) => format!("{refusal} {}", action.description()),
                None => refusal.to_owned(),
            };
            self.set_status(message, true);
            return;
        };
        let edits = self.edits.bank_edits();
        let cancel = Arc::new(AtomicBool::new(false));
        self.save_job = Some(SaveJob {
            target: target.clone(),
            cancel: Arc::clone(&cancel),
            files: edits.len(),
            then,
        });
        self.set_status("Saving…", false);
        let sender = self.sender.clone();
        let repaint = ctx.clone();

        thread::spawn(move || {
            let result = edit::save(&root, &edits, &target, &cancel);
            let _ = sender.send(WorkerMessage::Saved { result });
            repaint.request_repaint();
        });
    }

    /// Cancels the save in progress, if it hasn't started writing yet.
    pub(super) fn cancel_save(&mut self) {
        if let Some(job) = &self.save_job {
            job.cancel.store(true, Ordering::Relaxed);
        }
    }

    pub(super) fn finish_save(&mut self, ctx: &egui::Context, result: Result<SaveReport, Error>) {
        let Some(job) = self.save_job.take() else {
            return;
        };
        let report = match result {
            Ok(report) => report,
            Err(Error::Cancelled) => {
                self.set_status("Save cancelled. No files were changed.", false);
                return;
            }
            Err(error) => {
                self.set_status(
                    format!("Could not save. No files were changed: {error}"),
                    true,
                );
                return;
            }
        };
        let saved = saved_files(&report);
        match &job.target {
            SaveTarget::InPlace => {
                self.edits.saved_in_place(&report.saved);
                // The banks changed, so they're read again, unless what the
                // save was for is about to quit or open another game. If a
                // bank failed, that doesn't happen, and the app stays here.
                let leaving = job.then.is_some() && report.is_complete();
                if !report.saved.is_empty() && !leaving {
                    // Keep what's selected.
                    self.reselect = self.selected_bank().map(|bank| {
                        (
                            bank.path.clone(),
                            self.selected_segment().map(|segment| segment.id.clone()),
                        )
                    });
                    self.game_cache = Arc::default();
                    self.start_scan(ctx);
                }
                if report.is_complete() {
                    self.set_status(
                        format!(
                            "Saved {saved}. The game's own files are kept beside them, \
                             ending in .orig."
                        ),
                        false,
                    );
                }
            }
            SaveTarget::Folder(folder) => {
                if report.is_complete() {
                    self.edits.exported();
                    self.set_status(
                        format!("Saved {saved} to {}.", folder.join("Sounds").display()),
                        false,
                    );
                }
            }
        }
        if let Some(failure) = report.failures.first() {
            // A failure can stop several files that have to match each other.
            let failed = job.files.saturating_sub(report.saved.len());
            let failed_files = if failed == 1 { "file" } else { "files" };
            let not_done = job.then.as_ref().map_or("", GuardedAction::description);
            self.set_status(
                format!(
                    "Saved {saved}, but {failed} {failed_files} could not be saved: {failure} \
                     {not_done}"
                )
                .trim_end()
                .to_owned(),
                true,
            );
        } else if let Some(action) = job.then {
            self.perform(ctx, action);
        }
    }

    /// Asks for another game folder, then opens it, asking first about
    /// unsaved replacements. The folder is chosen first, so that cancelling
    /// the choice keeps them.
    pub(super) fn request_choose_root(&mut self, ctx: &egui::Context) {
        if self.save_job.is_some() {
            self.set_status("Wait for the save to finish.", true);
            return;
        }
        let Some(root) = rfd::FileDialog::new()
            .set_title("Select the Splinter Cell installation or Sounds folder")
            .pick_folder()
        else {
            return;
        };
        self.guard(ctx, GuardedAction::OpenRoot(root));
    }

    fn guard(&mut self, ctx: &egui::Context, action: GuardedAction) {
        if self.save_job.is_some() {
            self.set_status("Wait for the save to finish.", true);
        } else if self.edits.has_unsaved() {
            self.unsaved_prompt = Some(action);
        } else {
            self.perform(ctx, action);
        }
    }

    pub(super) fn resolve_unsaved(&mut self, ctx: &egui::Context, choice: UnsavedChoice) {
        let Some(action) = self.unsaved_prompt.take() else {
            return;
        };
        match choice {
            UnsavedChoice::Save => {
                // The question says when the sound maps change too.
                self.map_change_confirmed |= self.edits.changes_lengths();
                self.start_save(ctx, SaveTarget::InPlace, Some(action));
            }
            UnsavedChoice::Discard => {
                self.edits.clear();
                self.perform(ctx, action);
            }
            UnsavedChoice::Cancel => {}
        }
    }

    fn perform(&mut self, ctx: &egui::Context, action: GuardedAction) {
        match action {
            GuardedAction::OpenRoot(root) => self.open_root(ctx, root),
            GuardedAction::Quit => {
                self.closing = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }
    }

    /// Keeps the window open if closing it would lose unsaved replacements,
    /// and asks about them instead.
    pub(super) fn guard_close(&mut self, ctx: &egui::Context) {
        if self.closing || !ctx.input(|input| input.viewport().close_requested()) {
            return;
        }
        if self.save_job.is_some() || self.edits.has_unsaved() {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.guard(ctx, GuardedAction::Quit);
        }
    }

    /// Asks where FFmpeg is, for when it isn't found on its own.
    pub(super) fn choose_ffmpeg(&mut self) {
        let Some(path) = rfd::FileDialog::new()
            .set_title("Choose the FFmpeg program")
            .pick_file()
        else {
            return;
        };
        match Ffmpeg::locate(Some(&path)) {
            Ok(_) => {
                self.ffmpeg_path = Some(path);
                if let Some(draft) = &mut self.draft {
                    draft.error = None;
                }
                self.set_status("FFmpeg found.", false);
            }
            Err(error) => self.set_status(error.to_string(), true),
        }
    }
}

/// Makes `samples` fit `segment` as `fit` says, encodes them, and decodes the
/// result to play.
#[allow(clippy::too_many_arguments)]
fn prepare(
    root: &Path,
    bank_path: &str,
    segment: &Segment,
    ffmpeg: &Ffmpeg,
    source: &Path,
    samples: &[i16],
    fit: FitOptions,
    cancel: &AtomicBool,
) -> Result<Replacement, Error> {
    let stream = segment
        .replaceable_stream(bank_path)
        .ok_or(edit::CANT_REPLACE)?;
    let fitted = convert::fit(
        ffmpeg,
        samples,
        segment.sample_rate,
        segment.sample_count as usize,
        &fit,
        cancel,
    )?;
    if fitted.len() != segment.sample_count as usize {
        // Checked now, as a save with it would fail.
        match stream.format {
            StreamFormat::UbisoftAdpcm => {
                edit::check_length_change(root, stream.path, stream.offsets[0], cancel)
                    .map_err(cant_change_length)?
            }
            StreamFormat::Pcm16 => return Err(MUST_KEEP_LENGTH.into()),
        }
    }
    let encoded = edit::encode_replacement(root, bank_path, segment, &fitted, cancel)?;
    let sample_count = fitted.len() as u32;
    let preview = match stream.format {
        StreamFormat::UbisoftAdpcm => decoder::decode(&encoded, sample_count, cancel)?,
        // The game plays it at the segment's rate, which can differ from the
        // rate it's stored at, as an event can change its pitch.
        StreamFormat::Pcm16 => DecodedAudio {
            samples: fitted,
            sample_rate: segment.sample_rate,
            channels: 1,
        },
    };
    Ok(Replacement {
        source: source.to_owned(),
        fit,
        file: stream.path.to_owned(),
        offsets: stream.offsets.to_vec(),
        bank_kind: None,
        stream: encoded.into(),
        sample_count,
        changes_length: sample_count != segment.sample_count,
        preview: PlayableAudio::new(preview)?,
    })
}

/// What a save wrote, such as "2 banks and 1 sound map".
fn saved_files(report: &SaveReport) -> String {
    let banks = report
        .saved
        .iter()
        .filter(|path| !edit::is_sound_map(path))
        .count();
    let maps = report.saved.len() - banks + report.maps.len();
    let count = |count: usize, noun: &str| match count {
        1 => format!("1 {noun}"),
        count => format!("{count} {noun}s"),
    };
    match (banks, maps) {
        (banks, 0) => count(banks, "bank"),
        (0, maps) => count(maps, "sound map"),
        (banks, maps) => format!("{} and {}", count(banks, "bank"), count(maps, "sound map")),
    }
}

/// Why a sound stored in a map can't be replaced at another length.
pub(super) const MUST_KEEP_LENGTH: &str = "A sound stored in a sound map has to keep its \
     length, as another would move every sound after it. Trim, pad or stretch it to fit.";

/// Why a replacement can't be another length, with what to do instead.
fn cant_change_length(error: Error) -> Error {
    match error {
        Error::Cancelled => Error::Cancelled,
        Error::Failed(message) => format!(
            "It can't keep its own length. {message} Trim, pad or stretch it to fit instead."
        )
        .into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::test_support::{replacement, stream_segment};
    use sc_mod_tool_core::decoder::DecodedAudio;

    #[test]
    fn pending_edits_track_changes_by_bank() {
        let mut edits = PendingEdits::default();
        assert!(!edits.has_unsaved());
        edits.insert(
            "ENGLISH/1_1_0.LS0",
            "ENGLISH/1_1_0.LS0:0",
            replacement("ENGLISH/1_1_0.LS0", 0, 10),
        );
        edits.insert(
            "ENGLISH/1_1_0.LS0",
            "ENGLISH/1_1_0.LS0:200",
            replacement("ENGLISH/1_1_0.LS0", 200, 10),
        );
        edits.insert(
            "GERMAN/1_1_0.LS0",
            "GERMAN/1_1_0.LS0:0",
            replacement("GERMAN/1_1_0.LS0", 0, 10),
        );
        assert_eq!(edits.len(), 3);
        assert_eq!(edits.bank_len("ENGLISH/1_1_0.LS0", None), 2);
        assert!(edits.has_unsaved());

        let banks = edits.bank_edits();
        let paths: Vec<_> = banks.iter().map(|bank| bank.path.as_str()).collect();
        assert_eq!(paths, ["ENGLISH/1_1_0.LS0", "GERMAN/1_1_0.LS0"]);
        let offsets: Vec<_> = banks[0]
            .replacements
            .iter()
            .map(|replacement| replacement.offset)
            .collect();
        assert_eq!(offsets, [0, 200]);

        // Saving one bank in place leaves the other unsaved.
        edits.saved_in_place(&["GERMAN/1_1_0.LS0".to_owned()]);
        assert_eq!(edits.len(), 2);
        assert!(edits.has_unsaved());
        edits.saved_in_place(&["ENGLISH/1_1_0.LS0".to_owned()]);
        assert!(edits.is_empty());
        assert!(!edits.has_unsaved());
    }

    #[test]
    fn a_map_sounds_replacement_goes_into_its_streams_file() {
        let mut edits = PendingEdits::default();
        let file = "ENGLISH/Music_Common.LS0";
        edits.insert(
            "ENGLISH/MAPS.LM0",
            "ENGLISH/MAPS.LM0#7",
            replacement(file, 0, 10),
        );
        edits.insert(
            "ENGLISH/MAPS.LM0",
            "ENGLISH/MAPS.LM0#9",
            replacement(file, 400, 10),
        );
        edits.insert(
            "ENGLISH/1_1_0.LS0",
            "ENGLISH/1_1_0.LS0:0",
            replacement("ENGLISH/1_1_0.LS0", 0, 10),
        );
        assert_eq!(edits.file_count(), 2);
        let banks = edits.bank_edits();
        let files: Vec<_> = banks
            .iter()
            .map(|bank| (bank.path.as_str(), bank.replacements.len()))
            .collect();
        assert_eq!(files, [("ENGLISH/1_1_0.LS0", 1), (file, 2)]);

        edits.saved_in_place(&[file.to_owned()]);
        assert!(
            edits
                .get("ENGLISH/MAPS.LM0", "ENGLISH/MAPS.LM0#7")
                .is_none()
        );
        assert_eq!(edits.len(), 1);
        assert!(edits.has_unsaved());
    }

    #[test]
    fn reverting_everything_leaves_nothing_unsaved() {
        let mut edits = PendingEdits::default();
        edits.insert(
            "ENGLISH/1_1_0.LS0",
            "ENGLISH/1_1_0.LS0:0",
            replacement("ENGLISH/1_1_0.LS0", 0, 10),
        );
        edits.remove("ENGLISH/1_1_0.LS0", "ENGLISH/1_1_0.LS0:0");
        assert!(!edits.has_unsaved());
        edits.insert(
            "ENGLISH/1_1_0.LS0",
            "ENGLISH/1_1_0.LS0:0",
            replacement("ENGLISH/1_1_0.LS0", 0, 10),
        );
        edits.remove_bank("ENGLISH/1_1_0.LS0", None);
        assert!(!edits.has_unsaved());
    }

    #[test]
    fn changing_the_options_stops_preparing_the_old_ones() {
        let bank_path = "ENGLISH/1_1_0.LS0";
        let mut app = SCModToolApp::new(None);
        let mut draft = ReplacementDraft::prepared(bank_path, stream_segment(bank_path, 0, 10), 10);
        let cancel = Arc::new(AtomicBool::new(false));
        draft.job = Some(DraftJob {
            request: 1,
            cancel: Arc::clone(&cancel),
            then: AfterPrepare::Apply,
        });
        app.draft = Some(draft);
        app.set_draft_fit(FitOptions {
            start: 5,
            ..FitOptions::default()
        });
        assert!(cancel.load(Ordering::Relaxed));
        assert!(!app.draft.as_ref().unwrap().is_busy());
    }

    #[test]
    fn a_refused_save_says_what_didnt_happen() {
        let mut app = SCModToolApp::new(None);
        let ctx = egui::Context::default();
        app.root = Some(PathBuf::from("game"));
        app.edits.insert(
            "ENGLISH/1_1_0.LS0",
            "ENGLISH/1_1_0.LS0:0",
            replacement("ENGLISH/1_1_0.LS0", 0, 10),
        );
        app.scan_cancel = Some(Arc::default());
        app.unsaved_prompt = Some(GuardedAction::Quit);
        app.resolve_unsaved(&ctx, UnsavedChoice::Save);
        assert!(!app.closing);
        assert!(app.status.is_error);
        assert!(
            app.status.message.contains("didn't quit"),
            "{}",
            app.status.message
        );
        assert_eq!(app.edits.len(), 1);
    }

    #[test]
    fn saving_nothing_goes_ahead() {
        let mut app = SCModToolApp::new(None);
        let ctx = egui::Context::default();
        app.unsaved_prompt = Some(GuardedAction::Quit);
        app.resolve_unsaved(&ctx, UnsavedChoice::Save);
        assert!(app.closing);
    }

    #[test]
    fn exported_edits_are_kept_for_the_next_export() {
        let mut edits = PendingEdits::default();
        edits.insert(
            "ENGLISH/1_1_0.LS0",
            "ENGLISH/1_1_0.LS0:0",
            replacement("ENGLISH/1_1_0.LS0", 0, 10),
        );
        edits.insert(
            "GERMAN/1_1_0.LS0",
            "GERMAN/1_1_0.LS0:0",
            replacement("GERMAN/1_1_0.LS0", 0, 10),
        );
        edits.exported();
        assert!(!edits.has_unsaved());
        assert_eq!(edits.len(), 2);

        // Reverting is a change too, and removes empty banks.
        edits.remove("ENGLISH/1_1_0.LS0", "ENGLISH/1_1_0.LS0:0");
        assert!(edits.has_unsaved());
        assert_eq!(edits.len(), 1);
        let paths: Vec<_> = edits
            .bank_edits()
            .into_iter()
            .map(|bank| bank.path)
            .collect();
        assert_eq!(paths, ["GERMAN/1_1_0.LS0"]);
    }

    #[test]
    fn applying_a_draft_adds_its_replacement() {
        let mut app = SCModToolApp::new(None);
        let ctx = egui::Context::default();
        let bank_path = "ENGLISH/1_1_0.LS0";
        app.draft = Some(ReplacementDraft::prepared(
            bank_path,
            stream_segment(bank_path, 64, 10),
            10,
        ));
        app.apply_draft(&ctx);
        assert!(app.draft.is_none());
        assert_eq!(
            app.replacement(bank_path, "ENGLISH/1_1_0.LS0:64")
                .map(|replacement| replacement.offsets.clone()),
            Some(vec![64])
        );
        assert!(app.edits.has_unsaved());
    }

    #[test]
    fn replacements_of_another_length_are_noted() {
        let mut app = SCModToolApp::new(None);
        let ctx = egui::Context::default();
        let bank_path = "ENGLISH/1_1_0.LS0";
        app.draft = Some(ReplacementDraft::prepared(
            bank_path,
            stream_segment(bank_path, 0, 10),
            10,
        ));
        app.apply_draft(&ctx);
        assert!(!app.edits.changes_lengths());
        app.draft = Some(ReplacementDraft::prepared(
            bank_path,
            stream_segment(bank_path, 200, 10),
            12,
        ));
        app.apply_draft(&ctx);
        assert!(app.draft.is_none());
        assert_eq!(app.edits.len(), 2);
        assert!(app.edits.changes_lengths());
    }

    #[test]
    fn changing_the_options_needs_a_new_replacement() {
        let bank_path = "ENGLISH/1_1_0.LS0";
        let mut app = SCModToolApp::new(None);
        app.draft = Some(ReplacementDraft::prepared(
            bank_path,
            stream_segment(bank_path, 0, 10),
            10,
        ));
        assert!(app.draft.as_ref().unwrap().current().is_some());
        app.set_draft_fit(FitOptions {
            start: 5,
            ..FitOptions::default()
        });
        assert!(app.draft.as_ref().unwrap().current().is_none());
    }

    #[test]
    fn a_stale_draft_job_is_ignored() {
        let bank_path = "ENGLISH/1_1_0.LS0";
        let mut app = SCModToolApp::new(None);
        let mut draft = ReplacementDraft::prepared(bank_path, stream_segment(bank_path, 0, 10), 10);
        draft.prepared = None;
        draft.job = Some(DraftJob {
            request: 2,
            cancel: Arc::default(),
            then: AfterPrepare::Nothing,
        });
        app.draft = Some(draft);

        app.finish_draft_job(1, Err("an older job".into()));
        assert!(app.draft.as_ref().unwrap().error.is_none());
        app.finish_draft_job(2, Err("FFmpeg wasn't found.".into()));
        let draft = app.draft.as_ref().unwrap();
        assert_eq!(draft.error.as_deref(), Some("FFmpeg wasn't found."));
        assert!(!draft.is_busy());
    }

    #[test]
    fn unsaved_replacements_are_asked_about() {
        let mut app = SCModToolApp::new(None);
        let ctx = egui::Context::default();
        app.edits.insert(
            "ENGLISH/1_1_0.LS0",
            "ENGLISH/1_1_0.LS0:0",
            replacement("ENGLISH/1_1_0.LS0", 0, 10),
        );
        let other = GuardedAction::OpenRoot(PathBuf::from("other game"));
        app.guard(&ctx, other.clone());
        assert_eq!(app.unsaved_prompt, Some(other));
        app.resolve_unsaved(&ctx, UnsavedChoice::Cancel);
        assert_eq!(app.unsaved_prompt, None);
        assert_eq!(app.edits.len(), 1);

        app.unsaved_prompt = Some(GuardedAction::Quit);
        app.resolve_unsaved(&ctx, UnsavedChoice::Discard);
        assert!(app.edits.is_empty());
        assert!(app.closing);
    }

    #[test]
    fn saving_in_place_forgets_the_saved_banks_and_rescans() {
        let mut app = SCModToolApp::new(None);
        let ctx = egui::Context::default();
        // The scan this starts fails in the background, which doesn't matter.
        app.root = Some(PathBuf::from("game"));
        app.selected_language = Some("ENGLISH".to_owned());
        app.edits.insert(
            "ENGLISH/1_1_0.LS0",
            "ENGLISH/1_1_0.LS0:0",
            replacement("ENGLISH/1_1_0.LS0", 0, 10),
        );
        app.edits.insert(
            "ENGLISH/1_1_1.LS0",
            "ENGLISH/1_1_1.LS0:0",
            replacement("ENGLISH/1_1_1.LS0", 0, 10),
        );
        app.save_job = Some(SaveJob {
            target: SaveTarget::InPlace,
            cancel: Arc::default(),
            files: 2,
            then: None,
        });
        let cache = Arc::clone(&app.game_cache);
        let report = SaveReport {
            written: vec![PathBuf::from("game/Sounds/ENGLISH/1_1_0.LS0")],
            saved: vec!["ENGLISH/1_1_0.LS0".to_owned()],
            failures: vec!["Could not write ENGLISH/1_1_1.LS0".to_owned()],
            ..SaveReport::default()
        };
        app.finish_save(&ctx, Ok(report));

        assert!(app.save_job.is_none());
        assert_eq!(app.edits.len(), 1);
        assert_eq!(app.edits.bank_len("ENGLISH/1_1_1.LS0", None), 1);
        assert!(app.edits.has_unsaved());
        assert!(app.status.is_error);
        assert!(
            app.status.message.contains("but 1 file could not be saved"),
            "{}",
            app.status.message
        );
        // The banks are read again, without what was read before.
        assert!(!Arc::ptr_eq(&cache, &app.game_cache));
        assert!(app.scan_cancel.is_some());
    }

    #[test]
    fn exporting_keeps_the_replacements() {
        let mut app = SCModToolApp::new(None);
        let ctx = egui::Context::default();
        app.edits.insert(
            "ENGLISH/1_1_0.LS0",
            "ENGLISH/1_1_0.LS0:0",
            replacement("ENGLISH/1_1_0.LS0", 0, 10),
        );
        app.save_job = Some(SaveJob {
            target: SaveTarget::Folder(PathBuf::from("mod")),
            cancel: Arc::default(),
            files: 2,
            then: Some(GuardedAction::Quit),
        });
        let report = SaveReport {
            saved: vec!["ENGLISH/1_1_0.LS0".to_owned()],
            ..SaveReport::default()
        };
        app.finish_save(&ctx, Ok(report));
        assert_eq!(app.edits.len(), 1);
        assert!(!app.edits.has_unsaved());
        assert!(!app.status.is_error);
        // Saving was asked for before quitting, which can go ahead now.
        assert!(app.closing);

        // Saving in place before quitting doesn't read the banks again.
        let mut app = SCModToolApp::new(None);
        app.root = Some(PathBuf::from("game"));
        app.selected_language = Some("ENGLISH".to_owned());
        app.edits.insert(
            "ENGLISH/1_1_0.LS0",
            "ENGLISH/1_1_0.LS0:0",
            replacement("ENGLISH/1_1_0.LS0", 0, 10),
        );
        app.save_job = Some(SaveJob {
            target: SaveTarget::InPlace,
            cancel: Arc::default(),
            files: 2,
            then: Some(GuardedAction::Quit),
        });
        let report = SaveReport {
            saved: vec!["ENGLISH/1_1_0.LS0".to_owned()],
            ..SaveReport::default()
        };
        app.finish_save(&ctx, Ok(report));
        assert!(app.closing);
        assert!(app.scan_cancel.is_none());

        // If a bank fails, the app doesn't quit, so the saved ones are read
        // again.
        let mut app = SCModToolApp::new(None);
        app.root = Some(PathBuf::from("game"));
        app.selected_language = Some("ENGLISH".to_owned());
        app.edits.insert(
            "ENGLISH/1_1_0.LS0",
            "ENGLISH/1_1_0.LS0:0",
            replacement("ENGLISH/1_1_0.LS0", 0, 10),
        );
        app.edits.insert(
            "ENGLISH/1_1_1.LS0",
            "ENGLISH/1_1_1.LS0:0",
            replacement("ENGLISH/1_1_1.LS0", 0, 10),
        );
        app.save_job = Some(SaveJob {
            target: SaveTarget::InPlace,
            cancel: Arc::default(),
            files: 2,
            then: Some(GuardedAction::Quit),
        });
        let report = SaveReport {
            saved: vec!["ENGLISH/1_1_0.LS0".to_owned()],
            failures: vec!["Could not write ENGLISH/1_1_1.LS0".to_owned()],
            ..SaveReport::default()
        };
        app.finish_save(&ctx, Ok(report));
        assert!(!app.closing);
        assert!(
            app.status.message.contains("didn't quit"),
            "{}",
            app.status.message
        );
        assert!(app.scan_cancel.is_some());
    }

    #[test]
    fn saving_asks_again_before_changing_a_map() {
        let mut app = SCModToolApp::new(None);
        let ctx = egui::Context::default();
        app.save_confirmed = true;
        let mut longer = replacement("ENGLISH/1_1_0.LS0", 0, 12);
        longer.changes_length = true;
        app.edits
            .insert("ENGLISH/1_1_0.LS0", "ENGLISH/1_1_0.LS0:0", longer);
        app.request_save_in_place(&ctx);
        assert!(app.confirming_save);
        assert!(app.save_job.is_none());

        // Once confirmed, it isn't asked again.
        app.confirm_save(&ctx, false);
        assert!(!app.map_change_confirmed);
        app.root = Some(PathBuf::from("game"));
        app.confirm_save(&ctx, true);
        assert!(app.map_change_confirmed);
        app.save_job = None;
        app.request_save_in_place(&ctx);
        assert!(!app.confirming_save);
    }

    #[test]
    fn saving_says_when_a_map_changed() {
        let mut app = SCModToolApp::new(None);
        let ctx = egui::Context::default();
        app.save_job = Some(SaveJob {
            target: SaveTarget::Folder(PathBuf::from("mod")),
            cancel: Arc::default(),
            files: 2,
            then: None,
        });
        let report = SaveReport {
            saved: vec!["ENGLISH/1_1_0.LS0".to_owned()],
            maps: vec!["ENGLISH/MAPS.LM0".to_owned()],
            ..SaveReport::default()
        };
        app.finish_save(&ctx, Ok(report));
        assert!(
            app.status
                .message
                .starts_with("Saved 1 bank and 1 sound map to"),
            "{}",
            app.status.message
        );
    }

    #[test]
    fn saving_waits_for_scans() {
        let mut app = SCModToolApp::new(None);
        let ctx = egui::Context::default();
        app.root = Some(PathBuf::from("game"));
        app.edits.insert(
            "ENGLISH/1_1_0.LS0",
            "ENGLISH/1_1_0.LS0:0",
            replacement("ENGLISH/1_1_0.LS0", 0, 10),
        );
        app.scan_cancel = Some(Arc::default());
        app.save_in_place(&ctx);
        assert!(app.save_job.is_none());
        assert!(app.status.is_error);
    }

    /// Replaces a segment of a real game's first numbered bank with a tone,
    /// stretched to fit, then saves it to a folder, through the same workers
    /// the UI uses. It needs FFmpeg, and the game in `SC_GAME_DIR` or
    /// `../Splinter Cell`.
    #[test]
    #[ignore = "needs the game files and FFmpeg"]
    fn replaces_a_real_segment() {
        use std::time::{Duration, Instant};

        let root = std::env::var_os("SC_GAME_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../Splinter Cell"));
        let scratch = std::env::temp_dir().join(format!("sc-mod-tool-{}", std::process::id()));
        std::fs::create_dir_all(scratch.join("out")).unwrap();
        let ctx = egui::Context::default();
        let mut app = SCModToolApp::new(None);
        let wait = |app: &mut SCModToolApp, done: &dyn Fn(&SCModToolApp) -> bool| {
            let started = Instant::now();
            while !done(app) {
                assert!(
                    started.elapsed() < Duration::from_secs(180),
                    "timed out: {}",
                    app.status.message
                );
                std::thread::sleep(Duration::from_millis(20));
                app.handle_messages(&ctx);
            }
        };

        app.open_root(&ctx, root.clone());
        wait(&mut app, &|app| {
            !app.discovering_languages && app.scan_cancel.is_none() && !app.banks.is_empty()
        });
        let bank_index = app
            .banks
            .iter()
            .position(|bank| bank.sound_kind.is_none() && !bank.segments.is_empty())
            .unwrap();
        app.selected_bank = Some(bank_index);
        let bank_path = app.banks[bank_index].path.clone();
        let segment = app.banks[bank_index].segments[0].clone();

        // Two seconds of a tone, in stereo at 44.1 kHz.
        let source = scratch.join("tone.wav");
        let tone = DecodedAudio {
            samples: (0..88200)
                .flat_map(|index| {
                    let value = ((index as f64 / 15.0).sin() * 8000.0) as i16;
                    [value, value]
                })
                .collect(),
            sample_rate: 44100,
            channels: 2,
        };
        tone.write_wav(std::fs::File::create(&source).unwrap())
            .unwrap();

        app.open_draft(&ctx, bank_path.clone(), segment.clone(), source.clone());
        wait(&mut app, &|app| {
            app.draft.as_ref().is_some_and(|draft| !draft.is_busy())
        });
        let draft = app.draft.as_ref().unwrap();
        assert_eq!(draft.error, None);
        assert_eq!(draft.samples.as_ref().unwrap().len(), 72000);

        app.set_draft_fit(FitOptions {
            mode: sc_mod_tool_core::convert::FitMode::Stretch,
            ..FitOptions::default()
        });
        app.apply_draft(&ctx);
        wait(&mut app, &|app| {
            app.draft.is_none() || app.draft.as_ref().unwrap().error.is_some()
        });
        assert!(
            app.draft.is_none(),
            "{:?}",
            app.draft.as_ref().unwrap().error
        );
        let replacement = app.replacement(&bank_path, &segment.id).unwrap();
        assert_eq!(replacement.sample_count, segment.sample_count);

        app.start_save(&ctx, SaveTarget::Folder(scratch.join("out")), None);
        wait(&mut app, &|app| !app.is_saving());
        assert!(!app.status.is_error, "{}", app.status.message);
        assert!(!app.edits.has_unsaved());

        let sounds = std::fs::read_dir(&root)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .find(|path| path.file_name().unwrap().eq_ignore_ascii_case("sounds"))
            .unwrap();
        let original = std::fs::read(sounds.join(&bank_path)).unwrap();
        let saved = std::fs::read(scratch.join("out/Sounds").join(&bank_path)).unwrap();
        let (offset, length) = segment.stream_range().unwrap();
        let range = offset as usize..(offset + length) as usize;
        assert_eq!(saved.len(), original.len());
        assert_ne!(saved[range.clone()], original[range.clone()]);
        assert_eq!(saved[..range.start], original[..range.start]);
        assert_eq!(saved[range.end..], original[range.end..]);

        // Keeping the tone's own two seconds changes the sound map too.
        app.open_draft(&ctx, bank_path.clone(), segment.clone(), source);
        wait(&mut app, &|app| {
            app.draft.as_ref().is_some_and(|draft| !draft.is_busy())
        });
        app.set_draft_fit(FitOptions {
            mode: sc_mod_tool_core::convert::FitMode::KeepLength,
            ..FitOptions::default()
        });
        app.apply_draft(&ctx);
        wait(&mut app, &|app| {
            app.draft.is_none() || app.draft.as_ref().unwrap().error.is_some()
        });
        let replacement = app.replacement(&bank_path, &segment.id).unwrap();
        assert_eq!(replacement.sample_count, 72000);
        assert!(replacement.changes_length);
        app.start_save(&ctx, SaveTarget::Folder(scratch.join("out")), None);
        wait(&mut app, &|app| !app.is_saving());
        assert!(!app.status.is_error, "{}", app.status.message);
        assert!(
            app.status.message.contains("1 sound map"),
            "{}",
            app.status.message
        );
        let saved = std::fs::read(scratch.join("out/Sounds").join(&bank_path)).unwrap();
        assert_ne!(saved.len(), original.len());
        assert_eq!(saved[..range.start], original[..range.start]);
        let folder = bank_path.rsplit_once('/').unwrap().0;
        assert!(
            scratch
                .join("out/Sounds")
                .join(folder)
                .join("MAPS.LM0")
                .exists()
        );

        // One of Lambert's alarm warnings, a sound in the map whose stream is
        // in Music_Common.LS0.
        let find = |app: &SCModToolApp, format| {
            app.banks
                .iter()
                .filter(|bank| bank.sound_kind.is_some())
                .find_map(|bank| {
                    let segment = bank.segments.iter().find(|segment| {
                        segment
                            .replaceable_stream(&bank.path)
                            .is_some_and(|stream| stream.format == format)
                    })?;
                    Some((bank.path.clone(), segment.clone()))
                })
                .unwrap()
        };
        let (voices, alarm) = find(&app, sc_mod_tool_core::StreamFormat::UbisoftAdpcm);
        let file = alarm.replaceable_stream(&voices).unwrap().path.to_owned();
        assert!(file.ends_with("Music_Common.LS0"), "{file}");
        app.open_draft(
            &ctx,
            voices.clone(),
            alarm.clone(),
            scratch.join("tone.wav"),
        );
        wait(&mut app, &|app| {
            app.draft.as_ref().is_some_and(|draft| !draft.is_busy())
        });
        app.apply_draft(&ctx);
        wait(&mut app, &|app| {
            app.draft.is_none() || app.draft.as_ref().unwrap().error.is_some()
        });
        assert!(
            app.draft.is_none(),
            "{:?}",
            app.draft.as_ref().unwrap().error
        );
        assert_eq!(app.replacement(&voices, &alarm.id).unwrap().file, file);
        app.start_save(&ctx, SaveTarget::Folder(scratch.join("out")), None);
        wait(&mut app, &|app| !app.is_saving());
        assert!(!app.status.is_error, "{}", app.status.message);
        let original = std::fs::read(sounds.join(&file)).unwrap();
        let saved = std::fs::read(scratch.join("out/Sounds").join(&file)).unwrap();
        assert_eq!(saved.len(), original.len());
        assert_ne!(saved, original);

        // A sound effect stored in MAPS.SM0, in every area that has a copy.
        let (effects, effect) = find(&app, sc_mod_tool_core::StreamFormat::Pcm16);
        let stream = effect.replaceable_stream(&effects).unwrap();
        let (map, offsets, length) = (
            stream.path.to_owned(),
            stream.offsets.to_vec(),
            stream.byte_length as usize,
        );
        app.open_draft(
            &ctx,
            effects.clone(),
            effect.clone(),
            scratch.join("tone.wav"),
        );
        wait(&mut app, &|app| {
            app.draft.as_ref().is_some_and(|draft| !draft.is_busy())
        });
        app.apply_draft(&ctx);
        wait(&mut app, &|app| {
            app.draft.is_none() || app.draft.as_ref().unwrap().error.is_some()
        });
        assert!(
            app.draft.is_none(),
            "{:?}",
            app.draft.as_ref().unwrap().error
        );
        app.start_save(&ctx, SaveTarget::Folder(scratch.join("out")), None);
        wait(&mut app, &|app| !app.is_saving());
        assert!(!app.status.is_error, "{}", app.status.message);
        let original = std::fs::read(sounds.join(&map)).unwrap();
        let saved = std::fs::read(scratch.join("out/Sounds").join(&map)).unwrap();
        assert_eq!(saved.len(), original.len());
        let new = &saved[offsets[0] as usize..][..length];
        assert_ne!(new, &original[offsets[0] as usize..][..length]);
        for offset in offsets {
            assert_eq!(&saved[offset as usize..][..length], new);
        }
        std::fs::remove_dir_all(&scratch).unwrap();
    }
}
