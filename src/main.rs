mod app;
mod audio;

use std::env;
use std::fs;
use std::path::{self, PathBuf};

use eframe::egui;

fn main() -> eframe::Result<()> {
    // Optionally open a game folder straight away: `sc-mod-tool <game folder>`.
    // The folder is made absolute, so it can be reopened from anywhere next time.
    let startup_root = env::args_os().nth(1).map(|root| {
        fs::canonicalize(&root)
            .or_else(|_| path::absolute(&root))
            .unwrap_or_else(|_| root.into())
    });
    eframe::run_native(
        "SC Mod Tool",
        eframe::NativeOptions {
            renderer: eframe::Renderer::Wgpu,
            viewport: egui::ViewportBuilder::default()
                .with_inner_size([1200.0, 760.0])
                .with_min_inner_size([900.0, 560.0]),
            ..Default::default()
        },
        Box::new(|cc| {
            // Material icons look small next to text at their normal size.
            let mut icons = egui_material_icons::font_insert();
            icons.data.tweak.scale = 1.3;
            cc.egui_ctx.add_font(icons);
            // Show a pointing hand over buttons. Other clickable widgets set it
            // themselves.
            cc.egui_ctx.all_styles_mut(|style| {
                style.visuals.interact_cursor = Some(egui::CursorIcon::PointingHand);
            });
            // Otherwise reopen the folder from last time, if it is still there.
            let startup_root = startup_root.or_else(|| {
                let root = PathBuf::from(cc.storage?.get_string(app::ROOT_KEY)?);
                root.is_dir().then_some(root)
            });
            Ok(Box::new(app::SCModToolApp::new(startup_root)))
        }),
    )
}
