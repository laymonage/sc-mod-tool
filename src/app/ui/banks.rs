//! The list of sound banks on the left.

use eframe::egui::{self, Align, Align2, Layout, RichText};
use egui_material_icons::icons::{ICON_SEARCH, ICON_WARNING};
use sc_mod_tool_core::Bank;

use super::Action;
use crate::app::SCModToolApp;

impl SCModToolApp {
    pub(super) fn show_bank_panel(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        egui::Panel::left("banks")
            .resizable(true)
            .default_size(220.0)
            .size_range(160.0..=420.0)
            .show(ui, |ui| {
                ui.add_space(6.0);
                ui.label(RichText::new("Sound banks").strong());
                ui.add_space(4.0);
                ui.add(
                    egui::TextEdit::singleline(&mut self.bank_filter)
                        .hint_text(format!("{} Filter", ICON_SEARCH.codepoint))
                        .desired_width(f32::INFINITY),
                );
                ui.add_space(4.0);

                let filter = self.bank_filter.to_lowercase();
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.with_layout(Layout::top_down_justified(Align::LEFT), |ui| {
                            let mut shown = 0;
                            let mut shown_group: Option<&str> = None;
                            for (index, bank) in self.banks.iter().enumerate() {
                                if ![
                                    Some(&bank.name),
                                    bank.map.as_ref(),
                                    bank.mission_name.as_ref(),
                                ]
                                .into_iter()
                                .flatten()
                                .any(|text| text.to_lowercase().contains(&filter))
                                {
                                    continue;
                                }
                                shown += 1;
                                let group = mission_label(bank).unwrap_or("Other");
                                if shown_group != Some(group) {
                                    ui.add_space(6.0);
                                    ui.label(RichText::new(group).small().weak());
                                    shown_group = Some(group);
                                }
                                if bank_row(ui, bank, self.selected_bank == Some(index)).clicked() {
                                    actions.push(Action::SelectBank(index));
                                }
                            }
                            if shown == 0 {
                                ui.label(RichText::new("No banks match.").weak());
                            }
                        });
                    });
            });
    }
}

fn bank_row(ui: &mut egui::Ui, bank: &Bank, selected: bool) -> egui::Response {
    let name = if bank.segments.is_empty() {
        RichText::new(&bank.name).weak()
    } else {
        RichText::new(&bank.name)
    };
    let response = ui.selectable_label(selected, name);
    let (count, hint) = if bank.segments.is_empty() {
        (ICON_WARNING.codepoint.to_owned(), bank.issue.as_deref())
    } else {
        (bank.segments.len().to_string(), None)
    };
    ui.painter().text(
        response.rect.right_center() - egui::vec2(6.0, 0.0),
        Align2::RIGHT_CENTER,
        count,
        egui::TextStyle::Small.resolve(ui.style()),
        ui.visuals().weak_text_color(),
    );
    match hint {
        Some(hint) => response.on_hover_text(hint),
        None => response,
    }
}

/// The bank's mission name, or its extra map's folder when `HUD` doesn't name
/// it. The main sound maps' banks, which every level shares, are grouped
/// together.
pub(super) fn mission_label(bank: &Bank) -> Option<&str> {
    bank.mission_name
        .as_deref()
        .or(bank.map.as_deref())
        .or(bank.sound_kind.map(|_| "Sound maps"))
}
