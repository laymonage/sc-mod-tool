//! Small pieces the panels share.

use eframe::egui::{self, Response, RichText, Role, WidgetInfo};

/// A short status at the top of a card.
pub(super) fn badge(ui: &mut egui::Ui, text: &str) -> Response {
    egui::Frame::new()
        .fill(ui.visuals().widgets.inactive.weak_bg_fill)
        .corner_radius(4)
        .inner_margin(egui::Margin::symmetric(6, 1))
        .show(ui, |ui| ui.label(RichText::new(text).small()))
        .response
}

/// A [`detail_row`] in small text, for details within a card.
pub(super) fn small_detail_row(ui: &mut egui::Ui, name: &str, value: RichText) {
    ui.label(RichText::new(name).small().weak());
    ui.add(egui::Label::new(value.small()).wrap());
    ui.end_row();
}

pub(super) fn detail_row(ui: &mut egui::Ui, name: &str, value: impl Into<egui::WidgetText>) {
    ui.label(RichText::new(name).weak());
    ui.add(egui::Label::new(value).wrap());
    ui.end_row();
}

/// A top or bottom bar's frame, with the same space above and below its
/// contents.
pub(super) fn bar_frame(style: &egui::Style, vertical_margin: i8) -> egui::Frame {
    egui::Frame::side_top_panel(style).inner_margin(egui::Margin::symmetric(8, vertical_margin))
}

/// A segment action's button, with an icon before its label.
pub(super) fn action_button(ui: &mut egui::Ui, icon: &str, label: &str) -> Response {
    let button = ui.add(
        egui::Button::new((format!("{icon}  {label}"), egui::Atom::grow()))
            .min_size(egui::vec2(96.0, 28.0)),
    );
    // Named without its icon, which screen readers can't read.
    button.widget_info(|| WidgetInfo::labeled(Role::Button, true, label));
    button
}

/// Names an icon-only button for screen readers, which would otherwise read
/// its icon's private-use character, and shows the name as its tooltip.
pub(super) fn name_icon_button(button: Response, name: &str) -> Response {
    button.widget_info(|| WidgetInfo::labeled(Role::Button, true, name));
    button.on_hover_text(name)
}
