//! Renders the app with real game files, to look at its layout without
//! running it. Not run by default, as it needs a copy of the game:
//!
//! ```sh
//! SC_GAME_DIR="path/to/Splinter Cell" cargo test screenshots -- --ignored
//! ```
//!
//! The screenshots are saved in `target/screenshots`.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use std::{env, fs, thread};

use eframe::egui;
use egui_kittest::Harness;

use super::SCModToolApp;

const SCAN_TIMEOUT: Duration = Duration::from_secs(180);

#[test]
#[ignore = "needs the game files in SC_GAME_DIR"]
fn screenshots() {
    let root = PathBuf::from(env::var_os("SC_GAME_DIR").expect("SC_GAME_DIR isn't set"));
    let out = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/screenshots");
    fs::create_dir_all(&out).unwrap();

    let mut harness = Harness::builder()
        .with_size([1200.0, 760.0])
        .with_pixels_per_point(2.0)
        .wgpu()
        .build_eframe(|cc| {
            super::setup_context(&cc.egui_ctx);
            SCModToolApp::new(Some(root))
        });
    wait_for_scan(&mut harness);

    // A map's sound bank, which has the longest title and IDs.
    let bank = harness
        .state()
        .banks
        .iter()
        .position(|bank| bank.path.ends_with("1_7_1_1VselkaInfiltration/MAPS.SM0"))
        .expect("Vselka Infiltration's sound map bank wasn't found");
    harness.state_mut().select_bank(bank);
    save(&mut harness, &out, "map-bank");

    // The smallest window.
    harness.set_size(egui::vec2(900.0, 560.0));
    save(&mut harness, &out, "map-bank-narrow");

    // A voice bank, which has subtitles.
    let bank = harness
        .state()
        .banks
        .iter()
        .position(|bank| bank.sound_kind.is_none() && !bank.segments.is_empty())
        .expect("no voice bank was found");
    harness.set_size(egui::vec2(1200.0, 760.0));
    harness.state_mut().select_bank(bank);
    save(&mut harness, &out, "voice-bank");

    harness.ctx.set_theme(egui::Theme::Light);
    save(&mut harness, &out, "voice-bank-light");
}

fn wait_for_scan(harness: &mut Harness<SCModToolApp>) {
    let started = Instant::now();
    loop {
        harness.step();
        let app = harness.state();
        if !app.discovering_languages && app.scan_progress.is_none() && !app.banks.is_empty() {
            return;
        }
        assert!(
            started.elapsed() < SCAN_TIMEOUT,
            "the scan didn't finish: {}",
            app.status.message
        );
        thread::sleep(Duration::from_millis(50));
    }
}

fn save(harness: &mut Harness<SCModToolApp>, out: &Path, name: &str) {
    // Let panels and scroll areas settle on their sizes.
    harness.run_steps(4);
    let path = out.join(format!("{name}.png"));
    harness.render().unwrap().save(&path).unwrap();
    println!("Saved {}", path.display());
}
