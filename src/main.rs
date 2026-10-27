// Release builds on Windows don't open a console window beside the app.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

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
            app::setup_context(&cc.egui_ctx);
            // Otherwise reopen the folder from last time, if it is still there.
            let startup_root = startup_root.or_else(|| {
                let root = PathBuf::from(cc.storage?.get_string(app::ROOT_KEY)?);
                root.is_dir().then_some(root)
            });
            let ffmpeg = cc
                .storage
                .and_then(|storage| storage.get_string(app::FFMPEG_KEY))
                .map(PathBuf::from);
            Ok(Box::new(
                app::SCModToolApp::new(startup_root).with_ffmpeg_path(ffmpeg),
            ))
        }),
    )
}
