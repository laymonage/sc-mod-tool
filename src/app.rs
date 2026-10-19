mod ui;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::Duration;

use eframe::egui;
use sc_mod_tool_core::{
    Bank, Error, Language, Segment,
    decoder::DecodedAudio,
    subtitles::{CompiledLinks, Subtitle, SubtitleAnalysis},
};

use crate::audio::{ActivePlayback, AudioOutput};

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
        compiled_links: Option<Arc<CompiledLinks>>,
    },
    Playback {
        request: u64,
        segment_id: String,
        result: Result<DecodedAudio, Error>,
    },
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
    /// UELib's analysis of the selected game, reused when switching languages.
    compiled_links: Option<Arc<CompiledLinks>>,

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
    status: Status,

    bank_filter: String,
    segment_filter: String,
    text_filter: TextFilter,
    show_warnings: bool,
    /// Scroll the segment table to the selection on the next frame.
    scroll_to_selection: bool,
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
            compiled_links: None,
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
            status: Status {
                message: String::new(),
                is_error: false,
            },
            bank_filter: String::new(),
            segment_filter: String::new(),
            text_filter: TextFilter::All,
            show_warnings: false,
            scroll_to_selection: false,
        }
    }

    fn choose_root(&mut self, ctx: &egui::Context) {
        let Some(root) = rfd::FileDialog::new()
            .set_title("Select the Splinter Cell installation or Sounds folder")
            .pick_folder()
        else {
            return;
        };
        self.open_root(ctx, root);
    }

    fn open_root(&mut self, ctx: &egui::Context, root: PathBuf) {
        self.cancel_scan();
        self.stop_playback();
        self.scan_request = self.scan_request.wrapping_add(1);
        self.scan_cancel = None;
        self.scan_progress = None;
        self.root = Some(root.clone());
        self.languages.clear();
        self.selected_language = None;
        self.clear_banks();
        self.compiled_links = None;
        self.discovering_languages = true;
        self.language_request = self.language_request.wrapping_add(1);
        let request = self.language_request;
        let sender = self.sender.clone();
        let repaint = ctx.clone();
        self.set_status("Looking for sound banks...", false);

        thread::spawn(move || {
            let result = sc_mod_tool_core::discover_languages(&root);
            let _ = sender.send(WorkerMessage::Languages { request, result });
            repaint.request_repaint();
        });
    }

    fn select_language(&mut self, ctx: &egui::Context, language: String) {
        if self.selected_language.as_deref() != Some(&language) {
            self.selected_language = Some(language);
            self.start_scan(ctx);
        }
    }

    fn rescan(&mut self, ctx: &egui::Context) {
        // Rescanning re-reads the game files, including its scripts.
        self.compiled_links = None;
        self.start_scan(ctx);
    }

    fn start_scan(&mut self, ctx: &egui::Context) {
        let (Some(root), Some(language)) = (self.root.clone(), self.selected_language.clone())
        else {
            return;
        };

        self.cancel_scan();
        self.stop_playback();
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
        let mut compiled_links = self.compiled_links.clone();
        let sender = self.sender.clone();
        let repaint = ctx.clone();
        self.set_status("Scanning sound banks...", false);

        thread::spawn(move || {
            let send_progress = |progress| {
                let _ = sender.send(WorkerMessage::ScanProgress { request, progress });
                repaint.request_repaint();
            };
            let mut result = sc_mod_tool_core::discover_banks_with_progress(
                &root,
                &language,
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
                    reading_scripts: compiled_links.is_none(),
                });
                sc_mod_tool_core::subtitles::load_verified_subtitles(
                    &root,
                    &language,
                    banks,
                    &mut compiled_links,
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
                compiled_links,
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

    /// The segment that is playing, or being decoded to play.
    fn playing_segment_id(&self) -> Option<&str> {
        self.active_playback
            .as_ref()
            .map(|playback| playback.segment_id.as_str())
            .or_else(|| {
                self.pending_playback
                    .as_ref()
                    .map(|pending| pending.segment_id.as_str())
            })
    }

    /// Plays the segment, or stops it if it is already playing.
    fn toggle_playback(&mut self, ctx: &egui::Context, segment_index: usize) {
        let Some(segment) = self
            .selected_bank()
            .and_then(|bank| bank.segments.get(segment_index))
        else {
            return;
        };
        if self.playing_segment_id() == Some(segment.id.as_str()) {
            self.stop_playback();
            self.set_status("Playback stopped.", false);
        } else {
            self.play_segment(ctx, segment_index);
        }
    }

    fn play_segment(&mut self, ctx: &egui::Context, segment_index: usize) {
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
        self.stop_playback();
        self.playback_request = self.playback_request.wrapping_add(1);
        let request = self.playback_request;
        let segment_id = segment.id.clone();
        let cancelled = Arc::new(AtomicBool::new(false));
        self.pending_playback = Some(PendingPlayback {
            segment_id: segment_id.clone(),
            cancel: Arc::clone(&cancelled),
        });
        let sender = self.sender.clone();
        let repaint = ctx.clone();

        thread::spawn(move || {
            let result = sc_mod_tool_core::decode_segment(&root, &bank_path, &segment, &cancelled);
            let _ = sender.send(WorkerMessage::Playback {
                request,
                segment_id,
                result,
            });
            repaint.request_repaint();
        });
    }

    fn stop_playback(&mut self) {
        if let Some(pending) = self.pending_playback.take() {
            pending.cancel.store(true, Ordering::Relaxed);
        }
        self.playback_request = self.playback_request.wrapping_add(1);
        if let Some(playback) = self.active_playback.take() {
            playback.sink.stop();
        }
        self.seek_drag = None;
    }

    fn seek(&mut self, position: Duration) {
        self.seek_drag = None;
        if let Some(playback) = &self.active_playback
            && let Err(error) = playback.seek(position)
        {
            self.set_status(format!("Could not seek: {error}"), true);
        }
    }

    fn toggle_pause(&mut self) {
        if let Some(playback) = &self.active_playback {
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
                    compiled_links,
                } => {
                    if request != self.scan_request {
                        continue;
                    }
                    self.compiled_links = compiled_links;
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
                    result,
                } => {
                    if request != self.playback_request {
                        continue;
                    }
                    self.pending_playback = None;
                    match result {
                        Ok(audio) => match self.audio.play(audio, segment_id) {
                            Ok(playback) => self.active_playback = Some(playback),
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

    fn set_status(&mut self, message: impl Into<String>, is_error: bool) {
        self.status = Status {
            message: message.into(),
            is_error,
        };
    }
}

impl eframe::App for SCModToolApp {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if let Some(root) = self.startup_root.take() {
            self.open_root(ctx, root);
        }
        self.handle_messages(ctx);
        if self
            .active_playback
            .as_ref()
            .is_some_and(|playback| playback.sink.empty())
        {
            self.active_playback = None;
            self.seek_drag = None;
        }

        // Keep repainting while work is in progress, so finished playback is
        // noticed without waiting for input.
        if self.scan_progress.is_some()
            || self.discovering_languages
            || self.pending_playback.is_some()
            || self.active_playback.is_some()
        {
            ctx.request_repaint_after(Duration::from_millis(50));
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.show(ui);
    }

    fn on_exit(&mut self) {
        // eframe 0.36 crashes in AppKit's Touch Bar code when the window is
        // closed with its close button, because the window is torn down
        // while the event loop keeps running. Nothing needs saving, so end
        // the process before that happens.
        // https://github.com/emilk/egui/issues/8717
        #[cfg(target_os = "macos")]
        std::process::exit(0);
    }
}
