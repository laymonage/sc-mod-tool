use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use sc_mod_tool_core::{
    decoder::decode,
    discover_banks, discover_languages,
    subtitles::{MappingConfidence, load_verified_subtitles},
};

fn sounds_directory(game_root: &Path) -> PathBuf {
    if game_root
        .file_name()
        .is_some_and(|name| name.to_string_lossy().eq_ignore_ascii_case("sounds"))
    {
        return game_root.to_path_buf();
    }

    ["Sounds", "sounds"]
        .iter()
        .map(|name| game_root.join(name))
        .find(|path| path.is_dir())
        .unwrap_or_else(|| panic!("No Sounds directory found under {}", game_root.display()))
}

#[test]
#[ignore = "requires local Splinter Cell game files; set SC1_GAME_DIR"]
fn decodes_every_discovered_game_segment() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repository_root = manifest_dir
        .parent()
        .expect("core crate should be inside the app repository");
    let game_root = env::var_os("SC1_GAME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| repository_root.join("../Splinter Cell"));
    assert!(
        game_root.is_dir(),
        "Game directory not found at {}. Set SC1_GAME_DIR to override the default.",
        game_root.display()
    );
    let sounds = sounds_directory(&game_root);
    let languages = discover_languages(&game_root)
        .unwrap_or_else(|error| panic!("Could not discover game languages: {error}"));
    let cancelled = AtomicBool::new(false);
    let mut compiled_links = None;
    let mut total_banks = 0;
    let mut total_segments = 0;

    for language in languages {
        let banks = discover_banks(&game_root, &language.directory)
            .unwrap_or_else(|error| panic!("Could not scan {}: {error}", language.label));
        let subtitles =
            load_verified_subtitles(&game_root, &language.directory, &banks, &mut compiled_links)
                .unwrap_or_else(|error| {
                    panic!("Could not load {} subtitles: {error}", language.label)
                });
        if language.directory.eq_ignore_ascii_case("ENGLISH")
            || language.directory.eq_ignore_ascii_case("GERMAN")
        {
            assert!(
                subtitles.mapped_pairs > 0,
                "{}: expected direct compiled subtitle/audio links",
                language.label
            );
        }
        for (segment_id, possible_subtitles) in &subtitles.by_segment {
            assert!(
                banks
                    .iter()
                    .flat_map(|bank| &bank.segments)
                    .any(|segment| segment.id == *segment_id),
                "{}: subtitle mapping refers to an undiscovered segment",
                segment_id
            );
            for subtitle in possible_subtitles {
                assert!(
                    matches!(
                        subtitle.confidence,
                        MappingConfidence::ExplicitCompiledReference
                    ),
                    "{}: mapping is not based on an explicit compiled reference",
                    segment_id
                );
                assert!(!subtitle.text.trim().is_empty(), "{segment_id}: empty cue");
            }
        }
        let mut language_banks = 0;
        let mut language_segments = 0;

        for bank in banks.iter().filter(|bank| !bank.segments.is_empty()) {
            let bank_path = sounds.join(&bank.path);
            let bytes = fs::read(&bank_path)
                .unwrap_or_else(|error| panic!("Could not read {}: {error}", bank_path.display()));
            language_banks += 1;

            for segment in &bank.segments {
                let start = usize::try_from(segment.offset).unwrap_or_else(|_| {
                    panic!(
                        "{} at offset {}: segment offset does not fit in memory",
                        segment.id, segment.offset
                    )
                });
                let length = usize::try_from(segment.byte_length).unwrap_or_else(|_| {
                    panic!("{}: segment length does not fit in memory", segment.id)
                });
                let end = start
                    .checked_add(length)
                    .unwrap_or_else(|| panic!("{}: segment byte range overflows", segment.id));
                let input = bytes.get(start..end).unwrap_or_else(|| {
                    panic!(
                        "{}: segment range {start}..{end} exceeds {} bytes",
                        segment.id,
                        bytes.len()
                    )
                });
                let decoded = decode(input, segment.sample_count, &cancelled)
                    .unwrap_or_else(|error| panic!("{}: {error}", segment.id));

                assert_eq!(
                    decoded.samples.len(),
                    segment.sample_count as usize,
                    "{}: decoded sample count differs from scanner metadata",
                    segment.id
                );
                assert_eq!(
                    decoded.sample_rate, segment.sample_rate,
                    "{}: decoded sample rate differs from scanner metadata",
                    segment.id
                );
                language_segments += 1;
            }
        }

        println!(
            "{}: decoded {language_segments} segments from {language_banks} banks; mapped {}/{} explicit subtitle/audio references across {} segments with {} warnings",
            language.label,
            subtitles.mapped_pairs,
            subtitles.explicit_pairs,
            subtitles.by_segment.len(),
            subtitles.warnings.len()
        );
        for warning in subtitles.warnings.iter().take(8) {
            println!("  subtitle analysis warning: {warning}");
        }
        total_banks += language_banks;
        total_segments += language_segments;
    }

    assert!(
        total_segments > 0,
        "No supported audio segments were found under {}",
        game_root.display()
    );
    println!("Decoded {total_segments} segments from {total_banks} banks.");
}
