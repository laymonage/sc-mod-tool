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
        let subtitles = load_verified_subtitles(
            &game_root,
            &language.directory,
            &banks,
            &mut compiled_links,
            &cancelled,
        )
        .unwrap_or_else(|error| panic!("Could not load {} subtitles: {error}", language.label));
        assert!(
            subtitles.mapped_pairs > 0,
            "{}: expected direct compiled subtitle/audio links; are its localization files missing?",
            language.label
        );
        if language.directory.eq_ignore_ascii_case("GERMAN") {
            // German keeps leftover lines in this scene's section, so it's
            // aligned against English and its text looked up by cue name.
            let segment_id = "GERMAN/2_1_2.LS0:3932084";
            assert!(
                subtitles
                    .by_segment
                    .get(segment_id)
                    .is_some_and(|found| found.iter().any(|subtitle| subtitle.cue
                        == "Speech_0016L"
                        && subtitle
                            .text
                            .as_deref()
                            .is_some_and(|text| text.contains("Dein Freund?")))),
                "{segment_id}: expected German subtitle Speech_0016L"
            );
        }
        if language.directory.eq_ignore_ascii_case("ENGLISH") {
            // These packages store scrambled sound keys; the segments were
            // confirmed by listening.
            // The last two are spoken face to face, so the game doesn't show
            // them; their text comes from the pattern's localization section.
            for (segment_id, cue, confidence) in [
                (
                    "ENGLISH/0_0_2.LS0:449196",
                    "Speech_0012L",
                    MappingConfidence::ExplicitCompiledReference,
                ),
                (
                    "ENGLISH/0_0_2.LS0:564212",
                    "Speech_0018L",
                    MappingConfidence::ExplicitCompiledReference,
                ),
                (
                    "ENGLISH/2_1_2.LS0:1812880",
                    "Speech_0082L",
                    MappingConfidence::ExplicitCompiledReference,
                ),
                (
                    "ENGLISH/1_3_2.LS0:126924",
                    "Speech_0003L",
                    MappingConfidence::ExplicitCompiledReference,
                ),
                (
                    "ENGLISH/1_1_2.LS0:795812",
                    "Speech_0022L",
                    MappingConfidence::SectionOrder,
                ),
                (
                    "ENGLISH/1_1_2.LS0:1971596",
                    "Speech_0001L",
                    MappingConfidence::SectionOrder,
                ),
                (
                    "ENGLISH/0_0_3.LS0:10376",
                    "Speech_0009L",
                    MappingConfidence::SectionOrder,
                ),
                (
                    "ENGLISH/0_0.LS0:0",
                    "Speech_0007L",
                    MappingConfidence::SectionOrder,
                ),
                (
                    "ENGLISH/1_1_1.LS0:2316460",
                    "Speech_0001L",
                    MappingConfidence::SectionOrder,
                ),
                (
                    "ENGLISH/1_3_3.LS0:0",
                    "Speech_0011L",
                    MappingConfidence::SectionOrder,
                ),
                (
                    "ENGLISH/4_2_1.LS0:0",
                    "Speech_0001L",
                    MappingConfidence::SectionOrder,
                ),
            ] {
                assert!(
                    subtitles
                        .by_segment
                        .get(segment_id)
                        .is_some_and(|found| found.iter().any(
                            |subtitle| subtitle.cue == cue && subtitle.confidence == confidence
                        )),
                    "{segment_id}: expected {confidence:?} subtitle {cue}"
                );
            }
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
                if let Some(text) = &subtitle.text {
                    assert!(!text.trim().is_empty(), "{segment_id}: empty cue");
                    assert!(
                        !text.eq_ignore_ascii_case("(null)"),
                        "{segment_id}: placeholder shown as text"
                    );
                }
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
            "{}: decoded {language_segments} segments from {language_banks} banks; mapped {}/{} explicit subtitle/audio references and {} unshown subtitles; {} segments with text and {} with lines that have no text; {} warnings",
            language.label,
            subtitles.mapped_pairs,
            subtitles.explicit_pairs,
            subtitles.unshown_pairs,
            subtitles.segments_with_text(),
            subtitles.segments_with_empty_lines(),
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
