use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::Duration;

use eframe::egui;
use rodio::{OutputStream, OutputStreamHandle, Sink, buffer::SamplesBuffer};
use sc_mod_tool_core::{Bank, Language, Segment};

enum WorkerMessage {
    Languages {
        request: u64,
        result: Result<Vec<Language>, String>,
    },
    ScanProgress {
        request: u64,
        completed: usize,
        total: usize,
        bank: String,
    },
    Banks {
        request: u64,
        result: Result<Vec<Bank>, String>,
    },
    Playback {
        request: u64,
        segment_id: String,
        result: Result<sc_mod_tool_core::decoder::DecodedAudio, String>,
    },
}

struct ScanProgress {
    completed: usize,
    total: usize,
    bank: String,
}

struct ActivePlayback {
    _stream: OutputStream,
    sink: Sink,
    segment_id: String,
}

struct ScModToolApp {
    sender: Sender<WorkerMessage>,
    receiver: Receiver<WorkerMessage>,
    root: Option<PathBuf>,
    languages: Vec<Language>,
    selected_language: Option<String>,
    banks: Vec<Bank>,
    selected_bank: Option<usize>,
    language_request: u64,
    scan_request: u64,
    playback_request: u64,
    scan_cancel: Option<Arc<AtomicBool>>,
    playback_cancel: Option<Arc<AtomicBool>>,
    scan_progress: Option<ScanProgress>,
    active_playback: Option<ActivePlayback>,
    status: String,
    status_is_error: bool,
}

impl ScModToolApp {
    fn new() -> Self {
        let (sender, receiver) = mpsc::channel();
        Self {
            sender,
            receiver,
            root: None,
            languages: Vec::new(),
            selected_language: None,
            banks: Vec::new(),
            selected_bank: None,
            language_request: 0,
            scan_request: 0,
            playback_request: 0,
            scan_cancel: None,
            playback_cancel: None,
            scan_progress: None,
            active_playback: None,
            status: "Select the Splinter Cell installation or its Sounds folder.".to_owned(),
            status_is_error: false,
        }
    }

    fn choose_root(&mut self, ctx: &egui::Context) {
        let Some(root) = rfd::FileDialog::new()
            .set_title("Select the Splinter Cell installation or Sounds folder")
            .pick_folder()
        else {
            return;
        };

        self.cancel_scan();
        self.stop_playback();
        self.scan_request = self.scan_request.wrapping_add(1);
        self.scan_cancel = None;
        self.scan_progress = None;
        self.root = Some(root.clone());
        self.languages.clear();
        self.selected_language = None;
        self.banks.clear();
        self.selected_bank = None;
        self.scan_progress = None;
        self.language_request = self.language_request.wrapping_add(1);
        let request = self.language_request;
        let sender = self.sender.clone();
        let repaint = ctx.clone();
        self.set_status("Discovering language folders...", false);

        thread::spawn(move || {
            let result = sc_mod_tool_core::discover_languages(&root);
            let _ = sender.send(WorkerMessage::Languages { request, result });
            repaint.request_repaint();
        });
    }

    fn start_scan(&mut self, ctx: &egui::Context) {
        let (Some(root), Some(language)) = (self.root.clone(), self.selected_language.clone())
        else {
            return;
        };

        self.cancel_scan();
        self.stop_playback();
        self.banks.clear();
        self.selected_bank = None;
        self.scan_progress = Some(ScanProgress {
            completed: 0,
            total: 0,
            bank: String::new(),
        });
        self.scan_request = self.scan_request.wrapping_add(1);
        let request = self.scan_request;
        let cancelled = Arc::new(AtomicBool::new(false));
        self.scan_cancel = Some(Arc::clone(&cancelled));
        let sender = self.sender.clone();
        let repaint = ctx.clone();
        self.set_status(format!("Scanning {language} banks..."), false);

        thread::spawn(move || {
            let result = sc_mod_tool_core::discover_banks_with_progress(
                &root,
                &language,
                &cancelled,
                |completed, total, bank| {
                    let _ = sender.send(WorkerMessage::ScanProgress {
                        request,
                        completed,
                        total,
                        bank: bank.to_owned(),
                    });
                    repaint.request_repaint();
                },
            );
            let _ = sender.send(WorkerMessage::Banks { request, result });
            repaint.request_repaint();
        });
    }

    fn cancel_scan(&mut self) {
        if let Some(cancelled) = &self.scan_cancel {
            cancelled.store(true, Ordering::Relaxed);
            self.set_status("Cancelling bank scan...", false);
        }
    }

    fn play_segment(&mut self, ctx: &egui::Context, bank_path: String, segment: Segment) {
        let Some(root) = self.root.clone() else {
            return;
        };
        self.stop_playback();
        self.playback_request = self.playback_request.wrapping_add(1);
        let request = self.playback_request;
        let segment_id = segment.id.clone();
        let expected_samples = segment.sample_count;
        let cancelled = Arc::new(AtomicBool::new(false));
        self.playback_cancel = Some(Arc::clone(&cancelled));
        let sender = self.sender.clone();
        let repaint = ctx.clone();
        self.set_status(
            format!("Decoding {} at byte {}...", segment.bank, segment.offset),
            false,
        );

        thread::spawn(move || {
            let result = if cancelled.load(Ordering::Relaxed) {
                Err("Playback cancelled.".to_owned())
            } else {
                sc_mod_tool_core::read_segment_with_cancel(
                    &root,
                    &bank_path,
                    segment.offset,
                    segment.byte_length,
                    &cancelled,
                )
                .and_then(|bytes| {
                    if cancelled.load(Ordering::Relaxed) {
                        return Err("Playback cancelled.".to_owned());
                    }
                    sc_mod_tool_core::decoder::decode(&bytes, expected_samples, &cancelled)
                })
            };
            let _ = sender.send(WorkerMessage::Playback {
                request,
                segment_id,
                result,
            });
            repaint.request_repaint();
        });
    }

    fn stop_playback(&mut self) {
        if let Some(cancelled) = self.playback_cancel.take() {
            cancelled.store(true, Ordering::Relaxed);
        }
        self.playback_request = self.playback_request.wrapping_add(1);
        if let Some(playback) = self.active_playback.take() {
            playback.sink.stop();
        }
    }

    fn handle_messages(&mut self, ctx: &egui::Context) {
        while let Ok(message) = self.receiver.try_recv() {
            match message {
                WorkerMessage::Languages { request, result } => {
                    if request != self.language_request {
                        continue;
                    }
                    match result {
                        Ok(languages) if !languages.is_empty() => {
                            let first = languages[0].directory.clone();
                            self.languages = languages;
                            self.selected_language = Some(first);
                            self.set_status("Languages discovered. Scanning banks...", false);
                            self.start_scan(ctx);
                        }
                        Ok(_) => {
                            self.languages.clear();
                            self.set_status(
                                "No language folders with numeric .LS0 banks were found.",
                                true,
                            );
                        }
                        Err(error) => {
                            self.set_status(format!("Could not discover languages: {error}"), true)
                        }
                    }
                }
                WorkerMessage::ScanProgress {
                    request,
                    completed,
                    total,
                    bank,
                } => {
                    if request == self.scan_request {
                        self.scan_progress = Some(ScanProgress {
                            completed,
                            total,
                            bank,
                        });
                    }
                }
                WorkerMessage::Banks { request, result } => {
                    if request != self.scan_request {
                        continue;
                    }
                    self.scan_cancel = None;
                    self.scan_progress = None;
                    match result {
                        Ok(banks) => {
                            let supported_bank_count = banks
                                .iter()
                                .filter(|bank| !bank.segments.is_empty())
                                .count();
                            let segment_count: usize =
                                banks.iter().map(|bank| bank.segments.len()).sum();
                            self.selected_bank = banks
                                .iter()
                                .position(|bank| !bank.segments.is_empty())
                                .or_else(|| (!banks.is_empty()).then_some(0));
                            self.banks = banks;
                            self.set_status(
                                format!(
                                    "Scan complete: {} banks, {} supported segments in {} playable banks.",
                                    self.banks.len(),
                                    segment_count,
                                    supported_bank_count
                                ),
                                false,
                            );
                        }
                        Err(error) if error == "Scan cancelled." => {
                            self.set_status("Bank scan cancelled.", false);
                        }
                        Err(error) => {
                            self.banks.clear();
                            self.selected_bank = None;
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
                    self.playback_cancel = None;
                    match result {
                        Ok(audio) => match start_audio(audio, segment_id.clone()) {
                            Ok(playback) => {
                                self.active_playback = Some(playback);
                                self.set_status(format!("Playing segment {segment_id}."), false);
                            }
                            Err(error) => {
                                self.set_status(format!("Could not start playback: {error}"), true);
                            }
                        },
                        Err(error) if error == "Playback cancelled." => {}
                        Err(error) => {
                            self.set_status(format!("Could not decode segment: {error}"), true)
                        }
                    }
                }
            }
        }
    }

    fn set_status(&mut self, status: impl Into<String>, is_error: bool) {
        self.status = status.into();
        self.status_is_error = is_error;
    }

    fn show_toolbar(&mut self, ctx: &egui::Context) {
        let mut choose_root = false;
        let mut selected_language = self.selected_language.clone();
        let mut start_scan = false;
        let mut cancel_scan = false;
        let mut stop_playback = false;

        egui::TopBottomPanel::top("toolbar").show(ctx, |ui| {
            ui.horizontal(|ui| {
                if ui.button("Select game folder").clicked() {
                    choose_root = true;
                }
                if !self.languages.is_empty() {
                    egui::ComboBox::from_id_salt("language-selector")
                        .selected_text(
                            self.languages
                                .iter()
                                .find(|language| {
                                    Some(&language.directory) == selected_language.as_ref()
                                })
                                .map(|language| language.label.as_str())
                                .unwrap_or("Select language"),
                        )
                        .show_ui(ui, |ui| {
                            for language in &self.languages {
                                ui.push_id(&language.directory, |ui| {
                                    ui.selectable_value(
                                        &mut selected_language,
                                        Some(language.directory.clone()),
                                        &language.label,
                                    );
                                });
                            }
                        });
                    if ui.button("Rescan").clicked() {
                        start_scan = true;
                    }
                }
                if self.scan_progress.is_some() && ui.button("Cancel scan").clicked() {
                    cancel_scan = true;
                }
                if (self.active_playback.is_some() || self.playback_cancel.is_some())
                    && ui.button("Stop").clicked()
                {
                    stop_playback = true;
                }
            });
            if let Some(root) = &self.root {
                ui.small(format!("Game folder: {}", root.display()));
            }
        });

        if choose_root {
            self.choose_root(ctx);
        }
        if let Some(language) = selected_language {
            if self.selected_language.as_deref() != Some(&language) {
                self.selected_language = Some(language);
                self.start_scan(ctx);
            }
        }
        if start_scan {
            self.start_scan(ctx);
        }
        if cancel_scan {
            self.cancel_scan();
        }
        if stop_playback {
            self.stop_playback();
            self.set_status("Playback stopped.", false);
        }
    }

    fn show_banks(&mut self, ui: &mut egui::Ui) {
        ui.heading("Sound banks");
        if self.banks.is_empty() && self.scan_progress.is_none() {
            ui.label("Choose a game folder to discover supported banks.");
            return;
        }

        let mut selected_bank = None;
        egui::ScrollArea::vertical()
            .id_salt("bank-list-scroll")
            .show(ui, |ui| {
                for (index, bank) in self.banks.iter().enumerate() {
                    let label = if bank.segments.is_empty() {
                        format!("{}  ·  unsupported", bank.name)
                    } else {
                        format!("{}  ·  {}", bank.name, bank.segments.len())
                    };
                    let clicked = ui
                        .push_id(index, |ui| {
                            ui.selectable_label(self.selected_bank == Some(index), label)
                                .clicked()
                        })
                        .inner;
                    if clicked {
                        selected_bank = Some(index);
                    }
                }
            });
        if let Some(index) = selected_bank {
            self.stop_playback();
            self.selected_bank = Some(index);
        }
    }

    fn show_segments(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let Some(bank_index) = self.selected_bank else {
            ui.heading("Audio segments");
            ui.label("Select a bank to browse its supported streams.");
            return;
        };
        let Some(bank) = self.banks.get(bank_index) else {
            self.selected_bank = None;
            return;
        };

        let bank_name = bank.name.clone();
        let bank_path = bank.path.clone();
        let issue = bank.issue.clone();
        let segments = &bank.segments;
        ui.heading(&bank_name);
        ui.small(format!(
            "{} · {} bytes · {} supported segments",
            bank_path,
            bank.size,
            segments.len()
        ));
        if let Some(issue) = issue {
            ui.colored_label(egui::Color32::YELLOW, issue);
            return;
        }

        let mut play_segment = None;
        egui::ScrollArea::both()
            .id_salt("segment-list-scroll")
            .show(ui, |ui| {
                egui::Grid::new("segment-grid")
                    .striped(true)
                    .show(ui, |ui| {
                        for segment in segments {
                            let clicked = ui
                                .push_id(&segment.id, |ui| ui.button("Play").clicked())
                                .inner;
                            if clicked {
                                play_segment = Some(segment.clone());
                            }
                            ui.label(format!(
                                "{:.2} s",
                                segment.sample_count as f64 / segment.sample_rate as f64
                            ));
                            ui.label(format!(
                                "{} Hz · {} ch",
                                segment.sample_rate, segment.channels
                            ));
                            ui.label(format!("{} bytes", segment.byte_length));
                            ui.monospace(format!("offset {}", segment.offset));
                            ui.end_row();
                        }
                    });
            });
        if let Some(segment) = play_segment {
            self.play_segment(ctx, bank_path, segment);
        }
    }
}

impl eframe::App for ScModToolApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.handle_messages(ctx);
        if self
            .active_playback
            .as_ref()
            .is_some_and(|playback| playback.sink.empty())
        {
            if let Some(playback) = self.active_playback.take() {
                self.set_status(format!("Finished segment {}.", playback.segment_id), false);
            }
        }

        self.show_toolbar(ctx);
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.vertical(|ui| self.show_banks(ui));
                ui.separator();
                ui.vertical(|ui| self.show_segments(ui, ctx));
            });
            ui.separator();
            if let Some(progress) = &self.scan_progress {
                let fraction = if progress.total == 0 {
                    0.0
                } else {
                    progress.completed as f32 / progress.total as f32
                };
                ui.add(egui::ProgressBar::new(fraction).text(format!(
                    "{}/{} banks {}",
                    progress.completed, progress.total, progress.bank
                )));
            }
            ui.colored_label(
                if self.status_is_error {
                    egui::Color32::LIGHT_RED
                } else {
                    ui.visuals().text_color()
                },
                &self.status,
            );
            ui.small(
                "Read-only browser. Only recognized mono 4-bit Ubisoft ADPCM streams are playable.",
            );
        });

        if self.scan_progress.is_some() || self.playback_cancel.is_some() {
            ctx.request_repaint_after(Duration::from_millis(50));
        }
    }
}

fn start_audio(
    audio: sc_mod_tool_core::decoder::DecodedAudio,
    segment_id: String,
) -> Result<ActivePlayback, String> {
    let (stream, handle): (OutputStream, OutputStreamHandle) =
        OutputStream::try_default().map_err(|error| error.to_string())?;
    let sink = Sink::try_new(&handle).map_err(|error| error.to_string())?;
    sink.append(SamplesBuffer::new(1, audio.sample_rate, audio.samples));
    Ok(ActivePlayback {
        _stream: stream,
        sink,
        segment_id,
    })
}

fn main() -> eframe::Result<()> {
    eframe::run_native(
        "SC Mod Tool",
        eframe::NativeOptions::default(),
        Box::new(|_| Ok(Box::new(ScModToolApp::new()))),
    )
}
