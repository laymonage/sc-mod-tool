//! Replacing segments: the replacements made but not saved yet, preparing one
//! from an audio file, and saving them over the game's files or into a folder.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;

use eframe::egui;
use sc_mod_tool_core::convert::{self, Ffmpeg, FitOptions};
use sc_mod_tool_core::edit::{self, BankEdit, SaveReport, SaveTarget};
use sc_mod_tool_core::{Error, Segment, decoder};

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
    /// Where the stream it replaces starts in its bank.
    pub(super) offset: u64,
    pub(super) stream: Arc<[u8]>,
    pub(super) sample_count: u32,
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

    pub(super) fn remove_bank(&mut self, bank_path: &str) {
        if self.banks.remove(bank_path).is_some() {
            self.unsaved = true;
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

    /// How many of the bank's segments are replaced.
    pub(super) fn bank_len(&self, bank_path: &str) -> usize {
        self.banks.get(bank_path).map_or(0, BTreeMap::len)
    }

    /// Whether there are replacements that haven't been saved or exported
    /// since they were made or reverted.
    pub(super) fn has_unsaved(&self) -> bool {
        self.unsaved
    }

    fn bank_edits(&self) -> Vec<BankEdit> {
        self.banks
            .iter()
            .map(|(path, replacements)| BankEdit {
                path: path.clone(),
                replacements: replacements
                    .values()
                    .map(|replacement| edit::Replacement {
                        offset: replacement.offset,
                        stream: replacement.stream.to_vec(),
                    })
                    .collect(),
            })
            .collect()
    }

    /// Forgets the banks `saved` over the game's files, which now hold their
    /// replacements.
    fn saved_in_place(&mut self, saved: &[String]) {
        for path in saved {
            self.banks.remove(path);
        }
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
    /// What to do once everything is saved, after asking about unsaved
    /// replacements.
    then: Option<GuardedAction>,
}

/// Something that would lose unsaved replacements, so it's asked about first.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum GuardedAction {
    ChooseRoot,
    Quit,
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
        if segment.stream_range().is_none() {
            self.set_status("Only segments in numbered banks can be replaced.", true);
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
        self.draft = Some(ReplacementDraft {
            bank_path,
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

    pub(super) fn set_draft_fit(&mut self, fit: FitOptions) {
        if let Some(draft) = &mut self.draft {
            draft.fit = fit;
            draft.error = None;
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
        let Some(replacement) = draft.current().cloned() else {
            self.draft = Some(draft);
            return;
        };
        if replacement.sample_count != draft.segment.sample_count {
            self.draft = Some(ReplacementDraft {
                error: Some(
                    "Replacements of a different length can't be saved yet. Trim, pad or \
                     stretch the audio to fit."
                        .to_owned(),
                ),
                ..draft
            });
            return;
        }
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

    /// Removes all the bank's replacements.
    pub(super) fn revert_bank(&mut self, bank_path: &str) {
        if self
            .loaded_segment_id()
            .is_some_and(|id| id.starts_with(&format!("{bank_path}:")))
        {
            self.clear_playback();
        }
        self.edits.remove_bank(bank_path);
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

    fn start_save(&mut self, ctx: &egui::Context, target: SaveTarget, then: Option<GuardedAction>) {
        if self.save_job.is_some() {
            return;
        }
        let Some(root) = self.root.clone() else {
            return;
        };
        if self.edits.is_empty() {
            self.set_status("There's nothing to save.", false);
            return;
        }
        // A scan may have the files open.
        if self.scan_cancel.is_some() || self.discovering_languages {
            self.set_status("Wait for the scan to finish before saving.", true);
            return;
        }
        let edits = self.edits.bank_edits();
        let cancel = Arc::new(AtomicBool::new(false));
        self.save_job = Some(SaveJob {
            target: target.clone(),
            cancel: Arc::clone(&cancel),
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
        let saved = report.saved.len();
        let banks = if saved == 1 { "bank" } else { "banks" };
        match &job.target {
            SaveTarget::InPlace => {
                self.edits.saved_in_place(&report.saved);
                if saved > 0 {
                    // The banks changed, so read them again, keeping what's
                    // selected.
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
                            "Saved {saved} {banks}. The game's own files are kept beside them, \
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
                        format!(
                            "Saved {saved} {banks} to {}.",
                            folder.join("Sounds").display()
                        ),
                        false,
                    );
                }
            }
        }
        if let Some(failure) = report.failures.first() {
            let failed = report.failures.len();
            self.set_status(
                format!(
                    "Saved {saved} {banks}, but {failed} could not be saved, such as: {failure}"
                ),
                true,
            );
        } else if let Some(action) = job.then {
            self.perform(ctx, action);
        }
    }

    /// Opens another game folder, asking first if there are unsaved
    /// replacements.
    pub(super) fn request_choose_root(&mut self, ctx: &egui::Context) {
        self.guard(ctx, GuardedAction::ChooseRoot);
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
            UnsavedChoice::Save => self.start_save(ctx, SaveTarget::InPlace, Some(action)),
            UnsavedChoice::Discard => {
                self.edits.clear();
                self.perform(ctx, action);
            }
            UnsavedChoice::Cancel => {}
        }
    }

    fn perform(&mut self, ctx: &egui::Context, action: GuardedAction) {
        match action {
            GuardedAction::ChooseRoot => self.choose_root(ctx),
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
    let (offset, _) = segment
        .stream_range()
        .ok_or("Only segments in numbered banks can be replaced.")?;
    let fitted = convert::fit(
        ffmpeg,
        samples,
        segment.sample_rate,
        segment.sample_count as usize,
        &fit,
        cancel,
    )?;
    let stream = edit::encode_replacement(root, bank_path, segment, &fitted, cancel)?;
    let sample_count = fitted.len() as u32;
    let preview = decoder::decode(&stream, sample_count, cancel)?;
    Ok(Replacement {
        source: source.to_owned(),
        fit,
        offset,
        stream: stream.into(),
        sample_count,
        preview: PlayableAudio::new(preview)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::test_support::segment;
    use sc_mod_tool_core::SegmentSource;
    use sc_mod_tool_core::decoder::DecodedAudio;

    fn replacement(offset: u64, sample_count: u32) -> Replacement {
        Replacement {
            source: PathBuf::from("line.wav"),
            fit: FitOptions::default(),
            offset,
            stream: Arc::from(vec![offset as u8; 4]),
            sample_count,
            preview: PlayableAudio::new(DecodedAudio {
                samples: vec![0; sample_count as usize],
                sample_rate: 36000,
                channels: 1,
            })
            .unwrap(),
        }
    }

    fn stream_segment(bank_path: &str, offset: u64, sample_count: u32) -> Segment {
        Segment {
            id: format!("{bank_path}:{offset}"),
            source: SegmentSource::Stream {
                offset,
                byte_length: 100,
            },
            ..segment(sample_count, 36000)
        }
    }

    /// A draft for `segment`, with a replacement for its current options
    /// already prepared.
    fn prepared_draft(bank_path: &str, segment: Segment, sample_count: u32) -> ReplacementDraft {
        let offset = segment.stream_range().unwrap().0;
        ReplacementDraft {
            bank_path: bank_path.to_owned(),
            segment,
            source: PathBuf::from("line.wav"),
            samples: Some(Arc::new(vec![0; sample_count as usize])),
            fit: FitOptions::default(),
            prepared: Some(replacement(offset, sample_count)),
            error: None,
            job: None,
        }
    }

    #[test]
    fn pending_edits_track_changes_by_bank() {
        let mut edits = PendingEdits::default();
        assert!(!edits.has_unsaved());
        edits.insert(
            "ENGLISH/1_1_0.LS0",
            "ENGLISH/1_1_0.LS0:0",
            replacement(0, 10),
        );
        edits.insert(
            "ENGLISH/1_1_0.LS0",
            "ENGLISH/1_1_0.LS0:200",
            replacement(200, 10),
        );
        edits.insert("GERMAN/1_1_0.LS0", "GERMAN/1_1_0.LS0:0", replacement(0, 10));
        assert_eq!(edits.len(), 3);
        assert_eq!(edits.bank_len("ENGLISH/1_1_0.LS0"), 2);
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
    fn exported_edits_are_kept_for_the_next_export() {
        let mut edits = PendingEdits::default();
        edits.insert(
            "ENGLISH/1_1_0.LS0",
            "ENGLISH/1_1_0.LS0:0",
            replacement(0, 10),
        );
        edits.exported();
        assert!(!edits.has_unsaved());
        assert_eq!(edits.len(), 1);

        // Reverting is a change too, and removes empty banks.
        edits.remove("ENGLISH/1_1_0.LS0", "ENGLISH/1_1_0.LS0:0");
        assert!(edits.has_unsaved());
        assert!(edits.is_empty());
        assert!(edits.bank_edits().is_empty());
    }

    #[test]
    fn applying_a_draft_adds_its_replacement() {
        let mut app = SCModToolApp::new(None);
        let ctx = egui::Context::default();
        let bank_path = "ENGLISH/1_1_0.LS0";
        app.draft = Some(prepared_draft(
            bank_path,
            stream_segment(bank_path, 64, 10),
            10,
        ));
        app.apply_draft(&ctx);
        assert!(app.draft.is_none());
        assert_eq!(
            app.replacement(bank_path, "ENGLISH/1_1_0.LS0:64")
                .map(|replacement| replacement.offset),
            Some(64)
        );
        assert!(app.edits.has_unsaved());
    }

    #[test]
    fn replacements_of_another_length_are_kept_out() {
        let mut app = SCModToolApp::new(None);
        let ctx = egui::Context::default();
        let bank_path = "ENGLISH/1_1_0.LS0";
        app.draft = Some(prepared_draft(
            bank_path,
            stream_segment(bank_path, 0, 10),
            12,
        ));
        app.apply_draft(&ctx);
        let draft = app.draft.as_ref().unwrap();
        assert!(draft.error.as_deref().unwrap().contains("different length"));
        assert!(app.edits.is_empty());
    }

    #[test]
    fn changing_the_options_needs_a_new_replacement() {
        let bank_path = "ENGLISH/1_1_0.LS0";
        let mut app = SCModToolApp::new(None);
        app.draft = Some(prepared_draft(
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
        let mut draft = prepared_draft(bank_path, stream_segment(bank_path, 0, 10), 10);
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
            replacement(0, 10),
        );
        app.request_choose_root(&ctx);
        assert_eq!(app.unsaved_prompt, Some(GuardedAction::ChooseRoot));
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
            replacement(0, 10),
        );
        app.edits.insert(
            "ENGLISH/1_1_1.LS0",
            "ENGLISH/1_1_1.LS0:0",
            replacement(0, 10),
        );
        app.save_job = Some(SaveJob {
            target: SaveTarget::InPlace,
            cancel: Arc::default(),
            then: None,
        });
        let cache = Arc::clone(&app.game_cache);
        let report = SaveReport {
            written: vec![PathBuf::from("game/Sounds/ENGLISH/1_1_0.LS0")],
            saved: vec!["ENGLISH/1_1_0.LS0".to_owned()],
            backups: Vec::new(),
            failures: vec!["Could not write ENGLISH/1_1_1.LS0".to_owned()],
        };
        app.finish_save(&ctx, Ok(report));

        assert!(app.save_job.is_none());
        assert_eq!(app.edits.len(), 1);
        assert_eq!(app.edits.bank_len("ENGLISH/1_1_1.LS0"), 1);
        assert!(app.edits.has_unsaved());
        assert!(app.status.is_error);
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
            replacement(0, 10),
        );
        app.save_job = Some(SaveJob {
            target: SaveTarget::Folder(PathBuf::from("mod")),
            cancel: Arc::default(),
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
    }

    #[test]
    fn saving_waits_for_scans() {
        let mut app = SCModToolApp::new(None);
        let ctx = egui::Context::default();
        app.root = Some(PathBuf::from("game"));
        app.edits.insert(
            "ENGLISH/1_1_0.LS0",
            "ENGLISH/1_1_0.LS0:0",
            replacement(0, 10),
        );
        app.scan_cancel = Some(Arc::default());
        app.save_in_place(&ctx);
        assert!(app.save_job.is_none());
        assert!(app.status.is_error);
    }
}
