use std::collections::HashMap;
use std::env;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;

use sc_mod_tool_core::{
    GameCache, decode_segment, discover_banks_with_progress, discover_languages,
    subtitles::{MappingConfidence, load_verified_subtitles},
};

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
    let languages = discover_languages(&game_root)
        .unwrap_or_else(|error| panic!("Could not discover game languages: {error}"));
    let cancelled = AtomicBool::new(false);
    // Like the app, languages after the first read the shared sound maps'
    // banks and UELib's analysis from the cache.
    let mut cache = GameCache::default();
    let mut total_banks = 0;
    let mut total_segments = 0;
    let mut map_languages = HashMap::new();

    for language in languages {
        let banks = discover_banks_with_progress(
            &game_root,
            &language.directory,
            &mut cache,
            &cancelled,
            |_, _, _| {},
        )
        .unwrap_or_else(|error| panic!("Could not scan {}: {error}", language.label));
        let subtitles = load_verified_subtitles(
            &game_root,
            &language.directory,
            &banks,
            &mut cache,
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
            // These sounds' events play audio filed under other keys in
            // MAPS.LM0, or audio listed with the map's sounds.
            // Those matched by section order are spoken face to face, so the
            // game doesn't show them; their text comes from the pattern's
            // localization section.
            for (segment_id, cue, confidence) in [
                // Lambert's alarm warning, in Music_Common.LS0, the longest
                // of the three.
                (
                    "ENGLISH/MAPS.LM0#1",
                    "Speech_0001L",
                    MappingConfidence::ExplicitCompiledReference,
                ),
                // "Of course.", whose event plays its neighbour's resource:
                // the 1.7-second clip, not the 4.7-second one.
                (
                    "ENGLISH/2_2_1.LS0:1912308",
                    "Speech_0008L",
                    MappingConfidence::ExplicitCompiledReference,
                ),
                // These were confirmed by listening.
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
            // Every language lists the sounds in the shared `MAPS.SM0`, so
            // they're decoded with the first.
            let first_language = map_languages
                .entry(bank.path.clone())
                .or_insert_with(|| language.directory.clone());
            if *first_language != language.directory {
                continue;
            }
            language_banks += 1;

            for segment in &bank.segments {
                let decoded = decode_segment(&game_root, &bank.path, segment, &cancelled)
                    .unwrap_or_else(|error| panic!("{}: {error}", segment.id));
                let channels = segment.channels.min(2) as usize;
                assert_eq!(
                    decoded.channels as usize, channels,
                    "{}: decoded channels differ from the listed ones",
                    segment.id
                );
                assert_eq!(
                    decoded.samples.len(),
                    segment.sample_count as usize * channels,
                    "{}: decoded sample count differs from the listed one",
                    segment.id
                );
                assert_eq!(
                    decoded.sample_rate, segment.sample_rate,
                    "{}: decoded sample rate differs from the listed one",
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
