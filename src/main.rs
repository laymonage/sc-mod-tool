mod app;
mod audio;

use std::env;
use std::path::PathBuf;

use eframe::egui;

fn main() -> eframe::Result<()> {
    // Optionally open a game folder straight away: `sc-mod-tool <game folder>`.
    let startup_root = env::args_os().nth(1).map(PathBuf::from);
    eframe::run_native(
        "SC Mod Tool",
        eframe::NativeOptions {
            renderer: eframe::Renderer::Wgpu,
            viewport: egui::ViewportBuilder::default()
                .with_inner_size([1200.0, 760.0])
                .with_min_inner_size([900.0, 560.0]),
            ..Default::default()
        },
        Box::new(|_| Ok(Box::new(app::SCModToolApp::new(startup_root)))),
    )
}
