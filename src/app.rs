mod edits;
mod format;
mod names;
#[cfg(test)]
mod screenshots;
mod segments;
#[cfg(test)]
mod test_support;
mod ui;

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::Duration;

use eframe::egui;
use sc_mod_tool_core::{
    Bank, Error, GameCache, Language, Segment,
    subtitles::{Subtitle, SubtitleAnalysis},
};

use crate::audio::{ActivePlayback, AudioOutput, PlayableAudio};
use edits::{GuardedAction, PendingEdits, Replacement, ReplacementDraft, SaveJob};

enum WorkerMessage {
    Languages {
        request: u64,
        result: Result<Vec<Language>, Error>,
    },
    ScanProgress {
        request: u64,
        progress: ScanProgress,
    },
    Banks {
        request: u64,
        result: Result<Vec<Bank>, Error>,
        subtitles: Result<SubtitleAnalysis, Error>,
    },
    Playback {
        request: u64,
        segment_id: String,
        original: bool,
        result: Result<PlayableAudio, Error>,
    },
    /// A replacement draft's file was read, and its replacement prepared if
    /// asked for.
    Draft {
        request: u64,
        result: Result<(Arc<Vec<i16>>, Option<Replacement>), Error>,
    },
    Saved {
        result: Result<sc_mod_tool_core::edit::SaveReport, Error>,
    },
    Export {
        label: String,
        destination: PathBuf,
        result: Result<(), Error>,
    },
    BankExportProgress {
        completed: usize,
    },
    BankExport {
        result: Result<BankExportSummary, Error>,
    },
}

/// A bank's segments being saved as WAV files, one after another.
struct BankExport {
    label: String,
    completed: usize,
    total: usize,
    cancel: Arc<AtomicBool>,
}

impl BankExport {
    fn is_cancelling(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }
}

/// What exporting a bank did.
struct BankExportSummary {
    folder: PathBuf,
    exported: usize,
    /// Each segment that couldn't be exported, with why.
    failures: Vec<String>,
    cancelled: bool,
}

enum ScanProgress {
    /// Reading the language's sound banks.
    Banks {
        completed: usize,
        total: usize,
        bank: String,
    },
    /// Matching the banks' segments to subtitles, after reading the game's
    /// scripts with UELib unless they were read before.
    Subtitles { reading_scripts: bool },
}

/// A segment that is being read and decoded before it plays.
struct PendingPlayback {
    segment_id: String,
    /// Whether it's the game's own audio for a replaced segment.
    original: bool,
    cancel: Arc<AtomicBool>,
}

/// Which segments the segment table shows.
#[derive(Clone, Copy, PartialEq, Eq)]
enum TextFilter {
    All,
    /// Segments with a subtitle that has text.
    WithText,
    /// Segments matched only to lines without text, or to no line at all.
    WithoutText,
}

impl TextFilter {
    const ALL: [TextFilter; 3] = [
        TextFilter::All,
        TextFilter::WithText,
        TextFilter::WithoutText,
    ];

    fn label(self) -> &'static str {
        match self {
            TextFilter::All => "All segments",
            TextFilter::WithText => "With text",
            TextFilter::WithoutText => "Without text",
        }
    }
}

/// What happens when a segment plays to the end.
#[derive(Clone, Copy, PartialEq, Eq)]
enum PlaybackMode {
    Once,
    /// Play the next segment in the table.
    Autoplay,
    /// Play the segment again.
    Repeat,
}

impl PlaybackMode {
    /// The mode the playback mode button changes to.
    fn next(self) -> Self {
        match self {
            PlaybackMode::Once => PlaybackMode::Autoplay,
            PlaybackMode::Autoplay => PlaybackMode::Repeat,
            PlaybackMode::Repeat => PlaybackMode::Once,
        }
    }

    fn label(self) -> &'static str {
        match self {
            PlaybackMode::Once => "Play once",
            PlaybackMode::Autoplay => "Autoplay",
            PlaybackMode::Repeat => "Repeat",
        }
    }

    /// What clicking the playback mode button does.
    fn action(self) -> &'static str {
        match self {
            PlaybackMode::Once => "Enable autoplay",
            PlaybackMode::Autoplay => "Enable repeat",
            PlaybackMode::Repeat => "Disable repeat",
        }
    }
}

/// A column the segment table can be sorted by.
#[derive(Clone, Copy, PartialEq, Eq)]
enum SortColumn {
    /// The bank's own order.
    Number,
    Duration,
    Size,
    Offset,
    UsedIn,
    /// The map entry that plays a map sound.
    Entry,
    Key,
    Name,
    Subtitle,
}

/// How the segment table is sorted.
#[derive(Clone, Copy, PartialEq, Eq)]
struct SegmentSort {
    column: SortColumn,
    descending: bool,
}

impl SegmentSort {
    /// Sorts by `column`, or reverses the order if it's already sorted by it.
    fn by(self, column: SortColumn) -> Self {
        Self {
            column,
            descending: self.column == column && !self.descending,
        }
    }
}

struct Status {
    message: String,
    is_error: bool,
}

pub struct SCModToolApp {
    sender: Sender<WorkerMessage>,
    receiver: Receiver<WorkerMessage>,

    root: Option<PathBuf>,
    /// A game folder to open once the app starts.
    startup_root: Option<PathBuf>,
    languages: Vec<Language>,
    selected_language: Option<String>,
    banks: Vec<Bank>,
    selected_bank: Option<usize>,
    /// Index into the selected bank's segments.
    selected_segment: Option<usize>,
    subtitles: HashMap<String, Vec<Subtitle>>,
    subtitle_warnings: Vec<String>,
    /// What scans of the selected game reuse when switching languages. Only
    /// scan workers lock it, one at a time, so a scan keeps what the scan it
    /// replaced had read. A new game or a rescan gets a new one.
    game_cache: Arc<Mutex<GameCache>>,

    language_request: u64,
    scan_request: u64,
    playback_request: u64,
    discovering_languages: bool,
    scan_cancel: Option<Arc<AtomicBool>>,
    scan_progress: Option<ScanProgress>,
    pending_playback: Option<PendingPlayback>,
    active_playback: Option<ActivePlayback>,
    /// Where the seeker is being dragged to, so it follows the pointer instead
    /// of the playback position until it is released.
    seek_drag: Option<f32>,
    audio: AudioOutput,
    /// Where the last segment or bank was exported, to start there next time.
    export_directory: Option<PathBuf>,
    /// Only one bank is exported at a time.
    bank_export: Option<BankExport>,
    status: Status,

    bank_filter: String,
    segment_filter: String,
    text_filter: TextFilter,
    /// Kept when switching banks. Banks without the column are shown in their
    /// own order.
    segment_sort: SegmentSort,
    show_warnings: bool,
    playback_mode: PlaybackMode,
    /// Scroll the segment table to the selection on the next frame.
    scroll_to_selection: bool,

    /// Replacements that aren't saved yet, for any of the game's banks.
    edits: PendingEdits,
    /// The replacement being made, if any.
    draft: Option<ReplacementDraft>,
    draft_request: u64,
    save_job: Option<SaveJob>,
    /// What's waiting for an answer about unsaved replacements.
    unsaved_prompt: Option<GuardedAction>,
    /// Set once the window may close, after asking about unsaved replacements.
    closing: bool,
    /// Where FFmpeg is, if the user chose it.
    ffmpeg_path: Option<PathBuf>,
    /// Where the last replacement audio was chosen from.
    source_directory: Option<PathBuf>,
    /// The bank and segment to select once the scan in progress finishes, by
    /// path and ID.
    reselect: Option<(String, Option<String>)>,
    /// Whether the user is being asked to confirm saving over the game's
    /// files.
    confirming_save: bool,
    /// Whether they've confirmed it, which is only asked once.
    save_confirmed: bool,
    /// The window's title, kept to change it only when it should.
    window_title: String,
}

impl SCModToolApp {
    pub fn new(startup_root: Option<PathBuf>) -> Self {
        let (sender, receiver) = mpsc::channel();
        Self {
            sender,
            receiver,
            root: None,
            startup_root,
            languages: Vec::new(),
            selected_language: None,
            banks: Vec::new(),
            selected_bank: None,
            selected_segment: None,
            subtitles: HashMap::new(),
            subtitle_warnings: Vec::new(),
            game_cache: Arc::default(),
            language_request: 0,
            scan_request: 0,
            playback_request: 0,
            discovering_languages: false,
            scan_cancel: None,
            scan_progress: None,
            pending_playback: None,
            active_playback: None,
            seek_drag: None,
            audio: AudioOutput::default(),
            export_directory: None,
            bank_export: None,
            status: Status {
                message: String::new(),
                is_error: false,
            },
            bank_filter: String::new(),
            segment_filter: String::new(),
            text_filter: TextFilter::All,
            segment_sort: SegmentSort {
                column: SortColumn::Number,
                descending: false,
            },
            show_warnings: false,
            playback_mode: PlaybackMode::Once,
            scroll_to_selection: false,
            edits: PendingEdits::default(),
            draft: None,
            draft_request: 0,
            save_job: None,
            unsaved_prompt: None,
            closing: false,
            ffmpeg_path: None,
            source_directory: None,
            reselect: None,
            confirming_save: false,
            save_confirmed: false,
            window_title: String::new(),
        }
    }

    /// Uses the FFmpeg the user chose before, if any.
    pub fn with_ffmpeg_path(mut self, path: Option<PathBuf>) -> Self {
        self.ffmpeg_path = path;
        self
    }

    fn open_root(&mut self, ctx: &egui::Context, root: PathBuf) {
        self.cancel_scan();
        self.clear_playback();
        // Replacements belong to the game they were made for.
        self.close_draft();
        self.edits.clear();
        self.reselect = None;
        self.scan_request = self.scan_request.wrapping_add(1);
        self.scan_cancel = None;
        self.scan_progress = None;
        self.root = Some(root.clone());
        self.languages.clear();
        self.selected_language = None;
        self.clear_banks();
        self.game_cache = Arc::default();
        self.discovering_languages = true;
        self.language_request = self.language_request.wrapping_add(1);
        let request = self.language_request;
        let sender = self.sender.clone();
        let repaint = ctx.clone();
        self.set_status("Looking for sound banks…", false);

        thread::spawn(move || {
            let result = sc_mod_tool_core::discover_languages(&root);
            let _ = sender.send(WorkerMessage::Languages { request, result });
            repaint.request_repaint();
        });
    }

    fn select_language(&mut self, ctx: &egui::Context, language: String) {
        if self.refuse_while_saving() {
            return;
        }
        let nothing_loaded = self.banks.is_empty() && self.scan_progress.is_none();
        if self.selected_language.as_deref() != Some(&language) || nothing_loaded {
            self.selected_language = Some(language);
            self.start_scan(ctx);
        }
    }

    fn rescan(&mut self, ctx: &egui::Context) {
        if self.refuse_while_saving() {
            return;
        }
        // Rescanning re-reads the game files, including its scripts and
        // shared sound maps.
        self.game_cache = Arc::default();
        self.start_scan(ctx);
    }

    /// Whether a save is in progress, which scanning would get in the way of,
    /// telling the user if so.
    fn refuse_while_saving(&mut self) -> bool {
        if self.is_saving() {
            self.set_status("Wait for the save to finish.", true);
        }
        self.is_saving()
    }

    fn start_scan(&mut self, ctx: &egui::Context) {
        let (Some(root), Some(language)) = (self.root.clone(), self.selected_language.clone())
        else {
            return;
        };

        self.cancel_scan();
        self.clear_playback();
        self.clear_banks();
        self.scan_progress = Some(ScanProgress::Banks {
            completed: 0,
            total: 0,
            bank: String::new(),
        });
        self.scan_request = self.scan_request.wrapping_add(1);
        let request = self.scan_request;
        let cancelled = Arc::new(AtomicBool::new(false));
        self.scan_cancel = Some(Arc::clone(&cancelled));
        let game_cache = Arc::clone(&self.game_cache);
        let sender = self.sender.clone();
        let repaint = ctx.clone();
        self.set_status("Scanning sound banks…", false);

        thread::spawn(move || {
            // A cancelled scan may still be using the cache, until UELib
            // finishes if it's running. What it read is then kept for this
            // one.
            let mut game_cache = game_cache.lock().unwrap_or_else(PoisonError::into_inner);
            let send_progress = |progress| {
                let _ = sender.send(WorkerMessage::ScanProgress { request, progress });
                repaint.request_repaint();
            };
            let mut result = sc_mod_tool_core::discover_banks_with_progress(
                &root,
                &language,
                &mut game_cache,
                &cancelled,
                |completed, total, bank| {
                    send_progress(ScanProgress::Banks {
                        completed,
                        total,
                        bank: bank.to_owned(),
                    });
                },
            );
            let subtitles = if let Ok(banks) = &result {
                send_progress(ScanProgress::Subtitles {
                    reading_scripts: !game_cache.has_read_scripts(),
                });
                sc_mod_tool_core::subtitles::load_verified_subtitles(
                    &root,
                    &language,
                    banks,
                    &mut game_cache,
                    &cancelled,
                )
            } else {
                Ok(SubtitleAnalysis::default())
            };
            // Subtitles are part of the scan, so cancelling them cancels it.
            if matches!(subtitles, Err(Error::Cancelled)) {
                result = Err(Error::Cancelled);
            }
            let _ = sender.send(WorkerMessage::Banks {
                request,
                result,
                subtitles,
            });
            repaint.request_repaint();
        });
    }

    fn cancel_scan(&mut self) {
        if let Some(cancelled) = &self.scan_cancel {
            cancelled.store(true, Ordering::Relaxed);
        }
    }

    fn is_cancelling_scan(&self) -> bool {
        self.scan_cancel
            .as_ref()
            .is_some_and(|cancelled| cancelled.load(Ordering::Relaxed))
    }

    /// What the scan in progress is doing, to show the user.
    fn scan_activity(&self) -> &'static str {
        if self.discovering_languages {
            return "Looking for sound banks…";
        }
        let reading_scripts = matches!(
            self.scan_progress,
            Some(ScanProgress::Subtitles {
                reading_scripts: true
            })
        );
        match (self.is_cancelling_scan(), &self.scan_progress) {
            // UELib can't be interrupted, so cancelling waits for it.
            (true, _) if reading_scripts => "Cancelling once the game's scripts are read…",
            (true, _) => "Cancelling scan…",
            (false, _) if reading_scripts => "Reading the game's scripts for subtitles…",
            (false, Some(ScanProgress::Subtitles { .. })) => "Matching subtitles…",
            (false, _) => "Scanning sound banks…",
        }
    }

    fn clear_banks(&mut self) {
        self.banks.clear();
        self.selected_bank = None;
        self.selected_segment = None;
        self.subtitles.clear();
        self.subtitle_warnings.clear();
        self.show_warnings = false;
    }

    fn select_bank(&mut self, index: usize) {
        if self.selected_bank != Some(index) {
            self.selected_bank = Some(index);
            self.select_first_segment();
        }
    }

    fn select_first_segment(&mut self) {
        self.selected_segment = self.visible_segments().first().copied();
        self.scroll_to_selection = true;
    }

    fn selected_bank(&self) -> Option<&Bank> {
        self.banks.get(self.selected_bank?)
    }

    fn selected_segment(&self) -> Option<&Segment> {
        self.selected_bank()?.segments.get(self.selected_segment?)
    }

    /// The segment that is playing or paused, or being decoded to play.
    fn playing_segment_id(&self) -> Option<&str> {
        self.active_playback
            .as_ref()
            .filter(|playback| !playback.stopped)
            .map(|playback| playback.segment_id.as_str())
            .or_else(|| self.pending_segment_id())
    }

    /// The segment shown in the status bar: the one playing, or the last one
    /// played.
    fn loaded_segment_id(&self) -> Option<&str> {
        self.active_playback
            .as_ref()
            .map(|playback| playback.segment_id.as_str())
            .or_else(|| self.pending_segment_id())
    }

    fn pending_segment_id(&self) -> Option<&str> {
        self.pending_playback
            .as_ref()
            .map(|pending| pending.segment_id.as_str())
    }

    /// Whether what's playing or loaded is the game's own audio for a replaced
    /// segment, rather than what the segment plays now.
    fn is_original_loaded(&self) -> bool {
        match (&self.pending_playback, &self.active_playback) {
            (Some(pending), _) => pending.original,
            (None, Some(playback)) => playback.original,
            (None, None) => false,
        }
    }

    /// Plays the segment, or stops it if it is already playing.
    fn toggle_playback(&mut self, ctx: &egui::Context, segment_index: usize) {
        let Some(segment) = self
            .selected_bank()
            .and_then(|bank| bank.segments.get(segment_index))
        else {
            return;
        };
        let current = !self.is_original_loaded();
        if current && self.playing_segment_id() == Some(segment.id.as_str()) {
            self.stop_playback();
            self.set_status("Playback stopped.", false);
        } else if current && self.loaded_segment_id() == Some(segment.id.as_str()) {
            // The last segment played is still loaded, so play it again.
            self.toggle_pause();
        } else {
            self.play_segment(ctx, segment_index);
        }
    }

    /// Pauses or resumes the segment if it is loaded, or plays it.
    fn play_or_pause(&mut self, ctx: &egui::Context, segment_index: usize) {
        let Some(segment) = self
            .selected_bank()
            .and_then(|bank| bank.segments.get(segment_index))
        else {
            return;
        };
        let is_loaded = self
            .active_playback
            .as_ref()
            .is_some_and(|playback| playback.segment_id == segment.id && !playback.original);
        let is_pending = self
            .pending_playback
            .as_ref()
            .is_some_and(|pending| pending.segment_id == segment.id && !pending.original);
        if is_loaded {
            self.toggle_pause();
        } else if !is_pending {
            self.play_segment(ctx, segment_index);
        }
    }

    /// Plays what the segment plays now: its replacement if it has one.
    fn play_segment(&mut self, ctx: &egui::Context, segment_index: usize) {
        let replacement = self.selected_bank().and_then(|bank| {
            let segment = bank.segments.get(segment_index)?;
            let replacement = self.replacement(&bank.path, &segment.id)?;
            Some((replacement.preview.clone(), segment.id.clone()))
        });
        match replacement {
            Some((preview, segment_id)) => self.play_preview(preview, segment_id),
            None => self.decode_and_play(ctx, segment_index, false),
        }
    }

    /// Plays the game's own audio for the segment `segment_id` in the bank at
    /// `bank_path`, even if it's replaced. The bank must be the one selected.
    fn play_original(&mut self, ctx: &egui::Context, bank_path: &str, segment_id: &str) {
        let Some(bank) = self.selected_bank().filter(|bank| bank.path == bank_path) else {
            return;
        };
        let Some(index) = bank
            .segments
            .iter()
            .position(|segment| segment.id == segment_id)
        else {
            return;
        };
        let original = self.replacement(bank_path, segment_id).is_some();
        self.selected_segment = Some(index);
        self.decode_and_play(ctx, index, original);
    }

    /// Plays audio that's already decoded, such as a replacement's.
    fn play_preview(&mut self, preview: PlayableAudio, segment_id: String) {
        self.clear_playback();
        match self.audio.play(preview, segment_id) {
            Ok(playback) => self.active_playback = Some(playback),
            Err(error) => self.set_status(format!("Could not start playback: {error}"), true),
        }
    }

    /// Reads and decodes the segment from its bank, then plays it. `original`
    /// marks it as the game's own audio for a replaced segment.
    fn decode_and_play(&mut self, ctx: &egui::Context, segment_index: usize, original: bool) {
        let Some(root) = self.root.clone() else {
            return;
        };
        let Some(bank) = self.selected_bank() else {
            return;
        };
        let Some(segment) = bank.segments.get(segment_index).cloned() else {
            return;
        };
        let bank_path = bank.path.clone();
        self.clear_playback();
        self.playback_request = self.playback_request.wrapping_add(1);
        let request = self.playback_request;
        let segment_id = segment.id.clone();
        let cancelled = Arc::new(AtomicBool::new(false));
        self.pending_playback = Some(PendingPlayback {
            segment_id: segment_id.clone(),
            original,
            cancel: Arc::clone(&cancelled),
        });
        let sender = self.sender.clone();
        let repaint = ctx.clone();

        thread::spawn(move || {
            let result = sc_mod_tool_core::decode_segment(&root, &bank_path, &segment, &cancelled)
                .and_then(|audio| PlayableAudio::new(audio).map_err(Error::from));
            let _ = sender.send(WorkerMessage::Playback {
                request,
                segment_id,
                original,
                result,
            });
            repaint.request_repaint();
        });
    }

    /// Asks where to save the segment, then decodes it and saves it there as
    /// a WAV file.
    fn export_segment(&mut self, ctx: &egui::Context, segment_index: usize) {
        let Some(root) = self.root.clone() else {
            return;
        };
        let Some(bank) = self.selected_bank() else {
            return;
        };
        let Some(segment) = bank.segments.get(segment_index).cloned() else {
            return;
        };
        let bank_path = bank.path.clone();
        let label = names::segment_label(bank, &segment);
        let mut dialog = rfd::FileDialog::new()
            .set_title("Export segment")
            .set_file_name(names::export_file_name(bank, &segment))
            .add_filter("WAV audio", &["wav"]);
        if let Some(directory) = &self.export_directory {
            dialog = dialog.set_directory(directory);
        }
        let Some(destination) = dialog.save_file() else {
            return;
        };
        self.export_directory = destination.parent().map(PathBuf::from);
        self.set_status(format!("Exporting {label}…"), false);
        let sender = self.sender.clone();
        let repaint = ctx.clone();

        thread::spawn(move || {
            // Exports aren't cancelled: they're saved wherever the user chose,
            // whatever the app goes on to show.
            let result = sc_mod_tool_core::export_segment(
                &root,
                &bank_path,
                &segment,
                &destination,
                &AtomicBool::new(false),
            );
            let _ = sender.send(WorkerMessage::Export {
                label,
                destination,
                result,
            });
            repaint.request_repaint();
        });
    }

    /// Asks for a folder, then saves each of the selected bank's segments as a
    /// WAV file in a folder named after the bank inside it.
    fn export_bank(&mut self, ctx: &egui::Context) {
        if self.bank_export.is_some() {
            return;
        }
        let Some(root) = self.root.clone() else {
            return;
        };
        let Some(bank) = self.selected_bank() else {
            return;
        };
        let label = names::bank_title(bank);
        let mut dialog = rfd::FileDialog::new().set_title(format!("Export {label} to a folder"));
        if let Some(directory) = &self.export_directory {
            dialog = dialog.set_directory(directory);
        }
        let Some(parent) = dialog.pick_folder() else {
            return;
        };
        let folder = parent.join(names::export_folder_name(bank));
        let bank_path = bank.path.clone();
        let files: Vec<_> = bank
            .segments
            .iter()
            .cloned()
            .zip(names::export_file_names(bank))
            .collect();
        let total = files.len();
        let cancelled = Arc::new(AtomicBool::new(false));
        self.export_directory = Some(parent);
        self.bank_export = Some(BankExport {
            label,
            completed: 0,
            total,
            cancel: Arc::clone(&cancelled),
        });
        let sender = self.sender.clone();
        let repaint = ctx.clone();

        thread::spawn(move || {
            let send = |message| {
                let _ = sender.send(message);
                repaint.request_repaint();
            };
            if let Err(error) = fs::create_dir_all(&folder) {
                send(WorkerMessage::BankExport {
                    result: Err(format!("Could not create {}: {error}", folder.display()).into()),
                });
                return;
            }
            let mut summary = BankExportSummary {
                folder,
                exported: 0,
                failures: Vec::new(),
                cancelled: false,
            };
            for (completed, (segment, file_name)) in files.into_iter().enumerate() {
                send(WorkerMessage::BankExportProgress { completed });
                match sc_mod_tool_core::export_segment(
                    &root,
                    &bank_path,
                    &segment,
                    &summary.folder.join(&file_name),
                    &cancelled,
                ) {
                    Ok(()) => summary.exported += 1,
                    Err(Error::Cancelled) => {
                        summary.cancelled = true;
                        break;
                    }
                    // One segment that can't be decoded shouldn't stop the
                    // rest from being exported.
                    Err(error) => summary.failures.push(format!("{file_name}: {error}")),
                }
            }
            send(WorkerMessage::BankExport {
                result: Ok(summary),
            });
        });
    }

    fn cancel_bank_export(&mut self) {
        if let Some(export) = &self.bank_export {
            export.cancel.store(true, Ordering::Relaxed);
        }
    }

    /// Stops playback, keeping the segment in the status bar so it can be
    /// played again.
    fn stop_playback(&mut self) {
        self.cancel_pending_playback();
        self.seek_drag = None;
        if let Some(playback) = &mut self.active_playback
            && !playback.stopped
            && let Err(error) = self.audio.stop(playback)
        {
            self.active_playback = None;
            self.set_status(format!("Could not stop playback: {error}"), true);
        }
    }

    /// Stops playback and removes the segment from the status bar.
    fn clear_playback(&mut self) {
        self.cancel_pending_playback();
        self.seek_drag = None;
        if let Some(playback) = self.active_playback.take() {
            playback.player.stop();
        }
    }

    fn cancel_pending_playback(&mut self) {
        if let Some(pending) = self.pending_playback.take() {
            pending.cancel.store(true, Ordering::Relaxed);
        }
        self.playback_request = self.playback_request.wrapping_add(1);
    }

    fn seek(&mut self, position: Duration) {
        self.seek_drag = None;
        if let Some(playback) = &mut self.active_playback
            && let Err(error) = self.audio.seek(playback, position)
        {
            self.set_status(format!("Could not seek: {error}"), true);
        }
    }

    /// Selects the segment and plays it, scrolling the table to it.
    fn skip_to(&mut self, ctx: &egui::Context, segment_index: usize) {
        self.selected_segment = Some(segment_index);
        self.scroll_to_selection = true;
        self.play_or_pause(ctx, segment_index);
    }

    /// Rewinds a segment that has played to the end, or plays it or the next
    /// one again, depending on the playback mode.
    fn finish_playback(&mut self, ctx: &egui::Context) {
        let next = self
            .active_playback
            .as_ref()
            .and_then(|playback| self.adjacent_segment(&playback.segment_id, 1));
        match (self.playback_mode, next) {
            (PlaybackMode::Repeat, _) => self.seek(Duration::ZERO),
            (PlaybackMode::Autoplay, Some(index)) => self.skip_to(ctx, index),
            _ => self.stop_playback(),
        }
    }

    fn toggle_pause(&mut self) {
        if let Some(playback) = &mut self.active_playback {
            playback.toggle_pause();
        }
    }

    fn handle_messages(&mut self, ctx: &egui::Context) {
        while let Ok(message) = self.receiver.try_recv() {
            match message {
                WorkerMessage::Languages { request, result } => {
                    if request != self.language_request {
                        continue;
                    }
                    self.discovering_languages = false;
                    match result {
                        Ok(languages) => {
                            self.selected_language =
                                languages.first().map(|language| language.directory.clone());
                            self.languages = languages;
                            self.start_scan(ctx);
                        }
                        Err(error) => {
                            self.set_status(format!("Could not find sound banks: {error}"), true)
                        }
                    }
                }
                WorkerMessage::ScanProgress { request, progress } => {
                    if request == self.scan_request {
                        self.scan_progress = Some(progress);
                    }
                }
                WorkerMessage::Banks {
                    request,
                    result,
                    subtitles,
                } => {
                    if request != self.scan_request {
                        continue;
                    }
                    self.scan_cancel = None;
                    self.scan_progress = None;
                    match result {
                        Ok(banks) => self.finish_scan(banks, subtitles),
                        Err(Error::Cancelled) => self.set_status("Scan cancelled.", false),
                        Err(error) => {
                            self.clear_banks();
                            self.set_status(format!("Could not scan banks: {error}"), true);
                        }
                    }
                }
                WorkerMessage::Playback {
                    request,
                    segment_id,
                    original,
                    result,
                } => {
                    if request != self.playback_request {
                        continue;
                    }
                    self.pending_playback = None;
                    match result {
                        Ok(audio) => match self.audio.play(audio, segment_id) {
                            Ok(mut playback) => {
                                playback.original = original;
                                self.active_playback = Some(playback);
                            }
                            Err(error) => {
                                self.set_status(format!("Could not start playback: {error}"), true)
                            }
                        },
                        Err(Error::Cancelled) => {}
                        Err(error) => {
                            self.set_status(format!("Could not decode segment: {error}"), true)
                        }
                    }
                }
                WorkerMessage::Export {
                    label,
                    destination,
                    result,
                } => match result {
                    Ok(()) => self.set_status(
                        format!("Exported {label} to {}.", destination.display()),
                        false,
                    ),
                    Err(error) => {
                        self.set_status(format!("Could not export {label}: {error}"), true)
                    }
                },
                WorkerMessage::BankExportProgress { completed } => {
                    if let Some(export) = &mut self.bank_export {
                        export.completed = completed;
                    }
                }
                WorkerMessage::BankExport { result } => {
                    if let Some(export) = self.bank_export.take() {
                        self.finish_bank_export(&export, result);
                    }
                }
                WorkerMessage::Draft { request, result } => {
                    self.finish_draft_job(request, result);
                }
                WorkerMessage::Saved { result } => self.finish_save(ctx, result),
            }
        }
    }

    fn finish_scan(&mut self, banks: Vec<Bank>, subtitles: Result<SubtitleAnalysis, Error>) {
        let subtitle_error = match subtitles {
            Ok(analysis) => {
                self.subtitles = analysis.by_segment;
                self.subtitle_warnings = analysis.warnings;
                None
            }
            Err(error) => Some(error),
        };
        self.selected_bank = banks.iter().position(|bank| !bank.segments.is_empty());
        self.banks = banks;
        self.select_first_segment();
        if let Some((bank_path, segment_id)) = self.reselect.take() {
            self.reselect_bank(&bank_path, segment_id.as_deref());
        }

        let count = |is_map: bool| -> usize {
            self.banks
                .iter()
                .filter(|bank| bank.sound_kind.is_some() == is_map)
                .map(|bank| bank.segments.len())
                .sum()
        };
        let found = format!(
            "Found {} voice segments and {} map sounds",
            count(false),
            count(true)
        );
        let with_text = self
            .subtitles
            .values()
            .filter(|subtitles| subtitles.iter().any(Subtitle::has_text))
            .count();
        let with_empty_lines = self.subtitles.len() - with_text;
        match subtitle_error {
            Some(error) => {
                self.set_status(format!("{found}. Subtitles are unavailable: {error}"), true)
            }
            None => self.set_status(
                format!(
                    "{found}. {with_text} segments have subtitles, and {with_empty_lines} have lines with no text."
                ),
                false,
            ),
        }
    }

    /// Selects the bank at `bank_path`, and the segment `segment_id` in it if
    /// it's there, such as after saving rereads them.
    fn reselect_bank(&mut self, bank_path: &str, segment_id: Option<&str>) {
        let Some(index) = self.banks.iter().position(|bank| bank.path == bank_path) else {
            return;
        };
        self.selected_bank = Some(index);
        self.select_first_segment();
        if let Some(segment) = segment_id.and_then(|id| {
            self.banks[index]
                .segments
                .iter()
                .position(|segment| segment.id == id)
        }) {
            self.selected_segment = Some(segment);
        }
    }

    fn finish_bank_export(
        &mut self,
        export: &BankExport,
        result: Result<BankExportSummary, Error>,
    ) {
        let label = &export.label;
        let summary = match result {
            Ok(summary) => summary,
            Err(error) => {
                self.set_status(format!("Could not export {label}: {error}"), true);
                return;
            }
        };
        let folder = summary.folder.display();
        let exported = summary.exported;
        let total = export.total;
        if summary.cancelled {
            self.set_status(
                format!(
                    "Export cancelled after saving {exported} of {total} segments to {folder}."
                ),
                false,
            );
        } else if let Some(first) = summary.failures.first() {
            let failed = summary.failures.len();
            self.set_status(
                format!(
                    "Exported {exported} of {total} segments to {folder}. {failed} could not be exported, such as {first}"
                ),
                true,
            );
        } else {
            self.set_status(format!("Exported {label} to {folder}."), false);
        }
    }

    /// Marks the window's title while there are unsaved replacements.
    fn update_title(&mut self, ctx: &egui::Context) {
        let title = if self.edits.has_unsaved() {
            let count = self.edits.len();
            let noun = if count == 1 {
                "replacement"
            } else {
                "replacements"
            };
            format!("SC Mod Tool ({count} unsaved {noun})")
        } else {
            "SC Mod Tool".to_owned()
        };
        if title != self.window_title {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            self.window_title = title;
        }
    }

    fn set_status(&mut self, message: impl Into<String>, is_error: bool) {
        self.status = Status {
            message: message.into(),
            is_error,
        };
    }
}

/// Sets up the fonts and style the app's UI expects.
pub fn setup_context(ctx: &egui::Context) {
    // Material icons look small next to text at their normal size.
    let mut icons = egui_material_icons::font_insert();
    icons.data.tweak.scale = 1.3;
    ctx.add_font(icons);
    // Show a pointing hand over buttons. Other clickable widgets set it
    // themselves.
    ctx.all_styles_mut(|style| {
        style.visuals.interact_cursor = Some(egui::CursorIcon::PointingHand);
    });
    // egui's weak text is under 3:1 against the background, and its dark
    // theme's normal text only just reaches 4.5:1. These are at least 4.5:1
    // (WCAG AA), including on striped table rows.
    ctx.style_mut_of(egui::Theme::Dark, |style| {
        style.visuals.widgets.noninteractive.fg_stroke.color = egui::Color32::from_gray(185);
        style.visuals.weak_text_color = Some(egui::Color32::from_gray(140));
    });
    ctx.style_mut_of(egui::Theme::Light, |style| {
        style.visuals.weak_text_color = Some(egui::Color32::from_gray(105));
    });
}

/// The storage key for the last game folder opened.
pub const ROOT_KEY: &str = "root";
/// The storage key for the FFmpeg the user chose.
pub const FFMPEG_KEY: &str = "ffmpeg";

impl eframe::App for SCModToolApp {
    /// Remembers the game folder, to open it again next time, and FFmpeg if
    /// the user chose it.
    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        if let Some(root) = self.root.as_ref().and_then(|root| root.to_str()) {
            storage.set_string(ROOT_KEY, root.to_owned());
        }
        if let Some(ffmpeg) = self.ffmpeg_path.as_ref().and_then(|path| path.to_str()) {
            storage.set_string(FFMPEG_KEY, ffmpeg.to_owned());
        }
    }

    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if let Some(root) = self.startup_root.take() {
            self.open_root(ctx, root);
        }
        self.handle_messages(ctx);
        self.guard_close(ctx);
        self.update_title(ctx);
        if self
            .active_playback
            .as_ref()
            .is_some_and(|playback| !playback.stopped && playback.player.empty())
        {
            self.finish_playback(ctx);
        }
        // A paused segment can't finish, so it doesn't need repainting.
        let is_playing = self
            .active_playback
            .as_ref()
            .is_some_and(|playback| !playback.stopped && !playback.player.is_paused());

        // Keep repainting while work is in progress, so finished playback is
        // noticed without waiting for input.
        if self.scan_progress.is_some()
            || self.discovering_languages
            || self.pending_playback.is_some()
            || self.draft.as_ref().is_some_and(ReplacementDraft::is_busy)
            || self.is_saving()
            || is_playing
        {
            ctx.request_repaint_after(Duration::from_millis(50));
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.show(ui);
    }
}
