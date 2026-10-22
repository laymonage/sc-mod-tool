//! Writes every line the game plays without a subtitle, with the cue it was
//! matched to, as JSON.
//!
//! ```sh
//! cargo run -p sc-mod-tool-core --release --example dump_unshown_lines -- <game dir> [output.json]
//! ```
//!
//! Lines are matched in English, the language scenes are aligned against. Cue
//! names are the same in every language, so a script that passes them to
//! `Localize` shows each player their own language's text.

use std::collections::HashMap;
use std::env;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::AtomicBool;

use encoding_rs::WINDOWS_1252;
use sc_mod_tool_core::{
    GameCache, discover_banks_with_progress, discover_languages, subtitles::load_verified_subtitles,
};
use serde_json::json;

fn main() -> ExitCode {
    let mut arguments = env::args_os().skip(1);
    let Some(game_root) = arguments.next().map(PathBuf::from) else {
        eprintln!("Usage: dump_unshown_lines <game dir> [output.json]");
        return ExitCode::FAILURE;
    };
    let output = arguments.next().map(PathBuf::from);
    match run(&game_root, output.as_deref()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}

fn run(game_root: &Path, output: Option<&Path>) -> Result<(), String> {
    let language = discover_languages(game_root)
        .map_err(|error| format!("Could not discover game languages: {error}"))?
        .into_iter()
        .find(|language| language.directory.eq_ignore_ascii_case("ENGLISH"))
        .ok_or("The game has no English sound folder.")?;
    let cancelled = AtomicBool::new(false);
    let mut cache = GameCache::default();
    let banks = discover_banks_with_progress(
        game_root,
        &language.directory,
        &mut cache,
        &cancelled,
        |_, _, _| {},
    )
    .map_err(|error| format!("Could not scan {}: {error}", language.label))?;
    let analysis = load_verified_subtitles(
        game_root,
        &language.directory,
        &banks,
        &mut cache,
        &cancelled,
    )
    .map_err(|error| format!("Could not load {} subtitles: {error}", language.label))?;

    let files = section_files(&game_root.join("system").join("LOCALIZATION"))?;
    let lines = analysis
        .unshown_lines
        .iter()
        .map(|line| {
            json!({
                "pattern": line.pattern,
                "localizationFile": files.get(&line.pattern.to_ascii_lowercase()),
                "sourceObject": line.source_object,
                "soundEvent": line.sound_event,
                "function": line.function,
                "occurrence": line.occurrence,
                "cue": line.cue,
                "text": line.text,
                "segments": line.segments,
            })
        })
        .collect::<Vec<_>>();
    let with_text = analysis
        .unshown_lines
        .iter()
        .filter(|line| line.text.is_some())
        .count();
    let json = serde_json::to_string_pretty(&json!({
        "language": language.directory,
        "lines": lines,
        "warnings": analysis.warnings,
    }))
    .map_err(|error| format!("Could not write JSON: {error}"))?;

    match output {
        Some(path) => fs::write(path, json)
            .map_err(|error| format!("Could not write {}: {error}", path.display()))?,
        None => writeln!(io::stdout(), "{json}")
            .map_err(|error| format!("Could not write output: {error}"))?,
    }
    eprintln!(
        "{} lines without a subtitle, {with_text} of them with text, in {} patterns. {} warnings.",
        lines.len(),
        analysis
            .unshown_lines
            .iter()
            .map(|line| line.pattern.as_str())
            .collect::<std::collections::HashSet<_>>()
            .len(),
        analysis.warnings.len()
    );
    Ok(())
}

/// The English localization file each section is in, by lowercase section
/// name, as the file name without its extension (`P_1_1_0Tbilisi`).
fn section_files(localization_root: &Path) -> Result<HashMap<String, String>, String> {
    let entries = fs::read_dir(localization_root).map_err(|error| {
        format!(
            "Could not read directory {}: {error}",
            localization_root.display()
        )
    })?;
    let mut paths = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("int"))
        })
        .collect::<Vec<_>>();
    paths.sort();
    let mut files = HashMap::new();
    for path in paths {
        let bytes = fs::read(&path)
            .map_err(|error| format!("Could not read {}: {error}", path.display()))?;
        let (text, _, _) = WINDOWS_1252.decode(&bytes);
        let stem = path
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        for line in text.lines() {
            if let Some(section) = line
                .trim()
                .strip_prefix('[')
                .and_then(|line| line.strip_suffix(']'))
            {
                files
                    .entry(section.to_ascii_lowercase())
                    .or_insert_with(|| stem.clone());
            }
        }
    }
    Ok(files)
}
