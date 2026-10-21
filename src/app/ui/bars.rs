//! The top bar, and the status bar with the playback controls.

use std::time::Duration;

use eframe::egui::{self, Align, CursorIcon, Layout, RichText, Role, WidgetInfo};
use egui_material_icons::icons::{
    ICON_AUTOPLAY, ICON_FOLDER_OPEN, ICON_PAUSE, ICON_PLAY_ARROW, ICON_REFRESH, ICON_REPEAT_ONE,
    ICON_SKIP_NEXT, ICON_SKIP_PREVIOUS, ICON_WARNING,
};

use super::Action;
use super::widgets::{bar_frame, name_icon_button};
use crate::app::format::format_time;
use crate::app::names::segment_label;
use crate::app::{PlaybackMode, SCModToolApp, ScanProgress};

impl SCModToolApp {
    pub(super) fn show_top_bar(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        let panel = egui::Panel::top("top-bar").frame(bar_frame(ui.style(), 8));
        panel.show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new("SC Mod Tool").strong().size(16.0));
                ui.separator();
                if ui
                    .button(format!("{} Open game folder…", ICON_FOLDER_OPEN.codepoint))
                    .clicked()
                {
                    actions.push(Action::ChooseRoot);
                }

                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if !self.languages.is_empty() {
                        let scanning = self.scan_progress.is_some();
                        if ui
                            .add_enabled(
                                !scanning,
                                egui::Button::new(format!("{} Rescan", ICON_REFRESH.codepoint)),
                            )
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
                            })
                            .response
                            .on_hover_cursor(CursorIcon::PointingHand);
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
        });
    }

    pub(super) fn show_status_bar(&self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        let panel = egui::Panel::bottom("status-bar").frame(bar_frame(ui.style(), 4));
        panel.show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if !self.subtitle_warnings.is_empty()
                        && ui
                            .button(
                                RichText::new(format!(
                                    "{} {} warnings",
                                    ICON_WARNING.codepoint,
                                    self.subtitle_warnings.len()
                                ))
                                .color(ui.visuals().warn_fg_color),
                            )
                            .clicked()
                    {
                        actions.push(Action::ToggleWarnings);
                    }
                    if let Some(id) = self.loaded_segment_id() {
                        let label = self.find_segment(id).map_or_else(
                            || id.to_owned(),
                            |(bank, segment)| segment_label(bank, segment),
                        );
                        self.show_playback_controls(ui, id, actions);
                        ui.label(label);
                        ui.separator();
                    }
                    if let Some(export) = &self.bank_export {
                        if export.is_cancelling() {
                            ui.label("Cancelling export…");
                        } else {
                            if ui.button("Cancel").clicked() {
                                actions.push(Action::CancelBankExport);
                            }
                            ui.add(
                                egui::ProgressBar::new(
                                    export.completed as f32 / export.total.max(1) as f32,
                                )
                                .desired_width(180.0)
                                .text(format!("{} / {} segments", export.completed, export.total)),
                            );
                            ui.label(format!("Exporting {}", export.label));
                        }
                        ui.spinner();
                        ui.separator();
                    }
                    // The status takes whatever space the controls leave.
                    ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
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
                                    if ui.button("Cancel").clicked() {
                                        actions.push(Action::CancelScan);
                                    }
                                    ui.add(egui::Label::new(RichText::new(bank).weak()).truncate());
                                }
                                _ => {
                                    ui.add(egui::Label::new(self.scan_activity()).truncate());
                                    if !self.is_cancelling_scan() && ui.button("Cancel").clicked() {
                                        actions.push(Action::CancelScan);
                                    }
                                }
                            }
                        } else if self.status.is_error {
                            ui.add(
                                egui::Label::new(
                                    RichText::new(format!(
                                        "{} {}",
                                        ICON_WARNING.codepoint, self.status.message
                                    ))
                                    .color(ui.visuals().error_fg_color),
                                )
                                .truncate(),
                            );
                        } else {
                            ui.add(egui::Label::new(&self.status.message).truncate());
                        }
                    });
                });
            });
        });
    }

    /// The controls for the segment `segment_id`. They're disabled while it's
    /// decoded, and keep their place so they don't move between segments.
    fn show_playback_controls(
        &self,
        ui: &mut egui::Ui,
        segment_id: &str,
        actions: &mut Vec<Action>,
    ) {
        let playback = self
            .active_playback
            .as_ref()
            .filter(|playback| playback.segment_id == segment_id);
        let (mut position, total) = match playback {
            Some(playback) => (
                self.seek_drag
                    .unwrap_or_else(|| playback.position().as_secs_f32()),
                playback.duration.as_secs_f32(),
            ),
            None => (
                0.0,
                self.find_segment(segment_id).map_or(0.0, |(_, segment)| {
                    segment.sample_count as f32 / segment.sample_rate.max(1) as f32
                }),
            ),
        };
        ui.label(
            RichText::new(format!(
                "{} / {}",
                format_time(position),
                format_time(total)
            ))
            .monospace(),
        );
        ui.spacing_mut().slider_width = 160.0;
        let response = ui
            .add_enabled(
                playback.is_some(),
                egui::Slider::new(&mut position, 0.0..=total).show_value(false),
            )
            .on_hover_and_drag_cursor(CursorIcon::PointingHand);
        // The slider has no visible label, so it's named for screen readers.
        response.widget_info(|| WidgetInfo::slider(true, f64::from(position), "Seek"));
        if response.dragged() {
            actions.push(Action::DragSeek(Some(position)));
        } else if response.drag_stopped() || response.changed() {
            actions.push(Action::Seek(Duration::from_secs_f32(position)));
        }

        // Laid out right to left.
        let mode = self.playback_mode;
        // Playing once is autoplay turned off.
        let icon = match mode {
            PlaybackMode::Once | PlaybackMode::Autoplay => ICON_AUTOPLAY.codepoint,
            PlaybackMode::Repeat => ICON_REPEAT_ONE.codepoint,
        };
        let button = ui.add(egui::Button::new(icon).selected(mode != PlaybackMode::Once));
        if mode != PlaybackMode::Once {
            // A dot in the corner too, so the mode being on doesn't rely on
            // colour. It's painted over the button, so the icon doesn't move.
            ui.painter().circle_filled(
                button.rect.right_top() + egui::vec2(-5.0, 5.0),
                1.5,
                ui.visuals().selection.stroke.color,
            );
        }
        // Named for its mode, as a toggle's name shouldn't change when it's
        // pressed, and its tooltip says what clicking does.
        button.widget_info(|| {
            WidgetInfo::selected(
                Role::Button,
                true,
                mode != PlaybackMode::Once,
                format!("Playback mode: {}", mode.label()),
            )
        });
        if button.on_hover_text(mode.action()).clicked() {
            actions.push(Action::CyclePlaybackMode);
        }
        let skip_button =
            |ui: &mut egui::Ui, actions: &mut Vec<Action>, offset, icon: &str, name| {
                let target = playback.and_then(|_| self.adjacent_segment(segment_id, offset));
                let button = ui.add_enabled(target.is_some(), egui::Button::new(icon));
                let button = name_icon_button(button, name);
                if button.clicked()
                    && let Some(index) = target
                {
                    actions.push(Action::SkipTo(index));
                }
                button
            };
        let next = skip_button(ui, actions, 1, ICON_SKIP_NEXT.codepoint, "Next");
        // Play and pause are the same size as the skip buttons, and the
        // spinner takes that size too, so nothing moves when it's swapped.
        let size = next.rect.size();
        match playback {
            Some(playback) => {
                let (icon, name) = if playback.player.is_paused() {
                    (ICON_PLAY_ARROW, "Play")
                } else {
                    (ICON_PAUSE, "Pause")
                };
                let button = ui.add(egui::Button::new(icon.codepoint).min_size(size));
                if name_icon_button(button, name).clicked() {
                    actions.push(Action::TogglePause);
                }
            }
            None => {
                ui.add_sized(size, egui::Spinner::new())
                    .on_hover_text("Loading…");
            }
        }
        skip_button(ui, actions, -1, ICON_SKIP_PREVIOUS.codepoint, "Previous");
    }
}
