mod alignment;
mod bridge;
pub(crate) mod localization;
pub(crate) mod map;

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use encoding_rs::WINDOWS_1252;

use crate::sounds_folder::{
    LanguageFolder, find_file, find_sounds_directory, known_language, map_language_folders,
    safe_child_path,
};
use crate::{Error, GameCache};
use alignment::match_unshown_subtitles;
use bridge::{BridgeLink, BridgePattern, scan_compiled_links};
use localization::{find_cue, line_text, localization_path, localization_sections};
use map::{SoundMap, parse_map_records, resolve_segments};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MappingConfidence {
    /// The script passes the cue straight alongside the sound, and the game
    /// shows it as a subtitle.
    ExplicitCompiledReference,
    /// The game plays the sound without showing a subtitle. Its text is the
    /// line at the same position in the localization section named after the
    /// script's pattern.
    SectionOrder,
}

#[derive(Clone, Debug)]
pub struct Subtitle {
    pub cue: String,
    /// The line's text, or `None` when the localization file has no text for
    /// it: the line exists in its scene, but reads `(null)` or is empty.
    pub text: Option<String>,
    /// The compiled script objects that reference this cue and sound.
    pub sources: Vec<String>,
    pub sound_event: String,
    pub sound_map_key: u32,
    pub confidence: MappingConfidence,
}

impl Subtitle {
    pub fn has_text(&self) -> bool {
        self.text.is_some()
    }
}

/// A script call that plays a line without a subtitle, matched to its line in
/// the localization section named after its pattern.
#[derive(Clone, Debug)]
pub struct UnshownLine {
    pub pattern: String,
    /// The compiled state or function the call is in.
    pub source_object: String,
    /// `Package.Event`, as the script names the sound.
    pub sound_event: String,
    /// The native function the sound is passed to, such as `__NFUN_1408__`.
    pub function: Option<String>,
    /// How many earlier dialogue calls in the same source object play the
    /// same sound.
    pub occurrence: usize,
    pub cue: String,
    pub text: Option<String>,
    /// The segments the sound plays, empty when it couldn't be resolved.
    pub segments: Vec<String>,
}

#[derive(Clone, Debug, Default)]
pub struct SubtitleAnalysis {
    pub by_segment: HashMap<String, Vec<Subtitle>>,
    pub warnings: Vec<String>,
    pub explicit_pairs: usize,
    pub mapped_pairs: usize,
    /// Sounds matched to text the game doesn't show, by section order.
    pub unshown_pairs: usize,
    /// Every call matched to text the game doesn't show, in script order.
    pub unshown_lines: Vec<UnshownLine>,
}

impl SubtitleAnalysis {
    /// Segments with at least one subtitle that has text.
    pub fn segments_with_text(&self) -> usize {
        self.by_segment
            .values()
            .filter(|subtitles| subtitles.iter().any(Subtitle::has_text))
            .count()
    }

    /// Segments matched only to lines without text.
    pub fn segments_with_empty_lines(&self) -> usize {
        self.by_segment.len() - self.segments_with_text()
    }
}

/// The game's direct compiled `Localize`/`Sound` references, found by UELib.
///
/// They are the same for every language, and finding them is the slow part of
/// loading subtitles, so they're kept in the [`GameCache`].
#[derive(Debug)]
pub(crate) struct CompiledLinks {
    links: Vec<BridgeLink>,
    issues: Vec<String>,
    patterns: Vec<BridgePattern>,
}

/// Matches `language`'s sound banks to subtitle cues.
///
/// Runs UELib's analysis of the game's scripts unless `cache` already has it,
/// and keeps it there.
///
/// Stops with [`Error::Cancelled`] once `cancelled` is set. UELib's analysis
/// can't be interrupted, so cancelling while it runs takes effect when it
/// finishes. Its result is still kept in `cache`.
pub fn load_verified_subtitles(
    root: &Path,
    language: &str,
    banks: &[super::Bank],
    cache: &mut GameCache,
    cancelled: &AtomicBool,
) -> Result<SubtitleAnalysis, Error> {
    if cancelled.load(Ordering::Relaxed) {
        return Err(Error::Cancelled);
    }
    let extension = known_language(language)
        .ok_or_else(|| {
            format!("Subtitle localization extension is not known for language {language}.")
        })?
        .localization_extension;

    let sounds = find_sounds_directory(root)?;
    let game_root = sounds.parent().ok_or_else(|| {
        format!(
            "Could not find the game directory above {}.",
            sounds.display()
        )
    })?;
    let mut folders = vec![LanguageFolder {
        map: None,
        relative_path: language.to_owned(),
    }];
    folders.extend(map_language_folders(&sounds, language)?);
    let mut maps = Vec::with_capacity(folders.len());
    let mut map_warnings = Vec::new();
    for folder in folders {
        let map_path = find_file(
            &safe_child_path(&sounds, &folder.relative_path)?,
            "MAPS.LM0",
        )?;
        let map_data = fs::read(&map_path)
            .map_err(|error| format!("Could not read {}: {error}", map_path.display()))?;
        let records = parse_map_records(&map_data).unwrap_or_else(|error| {
            map_warnings.push(format!(
                "{}/MAPS.LM0 {error}, so its sounds' subtitles aren't matched.",
                folder.relative_path
            ));
            HashMap::new()
        });
        let file_name = map_path.file_name().unwrap_or_default().to_string_lossy();
        maps.push(SoundMap {
            path: format!("{}/{file_name}", folder.relative_path),
            records,
            banks: banks.iter().filter(|bank| bank.map == folder.map).collect(),
        });
    }

    let bridge = match &cache.compiled_links {
        Some(links) => Arc::clone(links),
        None => {
            let result = scan_compiled_links(game_root, cancelled)?;
            cache
                .compiled_links
                .insert(Arc::new(CompiledLinks {
                    links: result.links,
                    issues: result.issues,
                    patterns: result.patterns,
                }))
                .clone()
        }
    };
    if cancelled.load(Ordering::Relaxed) {
        return Err(Error::Cancelled);
    }
    let mut analysis = SubtitleAnalysis {
        explicit_pairs: bridge.links.len(),
        ..SubtitleAnalysis::default()
    };
    for warning in map_warnings {
        push_warning(&mut analysis.warnings, warning);
    }
    for issue in bridge.issues.iter().take(12) {
        push_warning(&mut analysis.warnings, issue.clone());
    }
    if bridge.issues.len() > 12 {
        push_warning(
            &mut analysis.warnings,
            format!(
                "{} more UELib script-analysis issue(s) were omitted.",
                bridge.issues.len() - 12
            ),
        );
    }
    if bridge.links.is_empty() {
        push_warning(
            &mut analysis.warnings,
            "No direct compiled Localize/Sound references were found.".to_owned(),
        );
    }

    let localization_root = game_root.join("system").join("LOCALIZATION");
    let mut localization_cache = HashMap::<PathBuf, String>::new();
    for link in &bridge.links {
        if cancelled.load(Ordering::Relaxed) {
            return Err(Error::Cancelled);
        }
        let key = link.sound_map_key;
        let segments = match resolve_segments(&link.sound_event, key, &maps) {
            Ok(segments) => segments,
            Err(warning) => {
                push_warning(&mut analysis.warnings, warning);
                continue;
            }
        };

        let localization_path =
            match localization_path(&localization_root, &link.localization_file, extension) {
                Ok(path) => path,
                Err(warning) => {
                    push_warning(&mut analysis.warnings, warning);
                    continue;
                }
            };
        if !localization_cache.contains_key(&localization_path) {
            let bytes = fs::read(&localization_path).map_err(|error| {
                format!("Could not read {}: {error}", localization_path.display())
            })?;
            let (text, _, _) = WINDOWS_1252.decode(&bytes);
            localization_cache.insert(localization_path.clone(), text.into_owned());
        }
        let text = match find_cue(
            localization_cache
                .get(&localization_path)
                .expect("localization file was just loaded"),
            &link.section,
            &link.cue,
        ) {
            Ok(text) => text,
            Err(warning) => {
                push_warning(&mut analysis.warnings, warning);
                continue;
            }
        };

        for segment in segments {
            add_subtitle(
                &mut analysis,
                &segment.id,
                Subtitle {
                    cue: link.cue.clone(),
                    text: line_text(&text),
                    sources: vec![format!("{}: {}", link.source_package, link.source_object)],
                    sound_event: link.sound_event.clone(),
                    sound_map_key: key,
                    confidence: MappingConfidence::ExplicitCompiledReference,
                },
            );
        }
        analysis.mapped_pairs += 1;
    }

    match localization_sections(&localization_root, extension) {
        Ok(sections) => {
            // Scenes are aligned against English, the language they were written in.
            let english = if extension == "int" {
                None
            } else {
                localization_sections(&localization_root, "int").ok()
            };
            for pattern in &bridge.patterns {
                if cancelled.load(Ordering::Relaxed) {
                    return Err(Error::Cancelled);
                }
                match_unshown_subtitles(
                    &mut analysis,
                    pattern,
                    &sections,
                    english.as_ref().unwrap_or(&sections),
                    &maps,
                );
            }
        }
        Err(warning) => push_warning(&mut analysis.warnings, warning),
    }

    for subtitles in analysis.by_segment.values_mut() {
        // Subtitles with text come first, then those the game shows.
        subtitles.sort_by(|left, right| {
            (!left.has_text())
                .cmp(&!right.has_text())
                .then_with(|| {
                    (left.confidence != MappingConfidence::ExplicitCompiledReference)
                        .cmp(&(right.confidence != MappingConfidence::ExplicitCompiledReference))
                })
                .then_with(|| {
                    left.cue
                        .to_ascii_lowercase()
                        .cmp(&right.cue.to_ascii_lowercase())
                })
                .then_with(|| left.sound_event.cmp(&right.sound_event))
        });
    }
    analysis.warnings.sort();
    Ok(analysis)
}

/// Adds a subtitle to a segment, merging its sources into an identical one.
fn add_subtitle(analysis: &mut SubtitleAnalysis, segment_id: &str, subtitle: Subtitle) {
    let subtitles = analysis
        .by_segment
        .entry(segment_id.to_owned())
        .or_default();
    if let Some(existing) = subtitles.iter_mut().find(|existing| {
        existing.cue.eq_ignore_ascii_case(&subtitle.cue)
            && existing.text == subtitle.text
            && existing
                .sound_event
                .eq_ignore_ascii_case(&subtitle.sound_event)
            && existing.sound_map_key == subtitle.sound_map_key
            && existing.confidence == subtitle.confidence
    }) {
        for source in subtitle.sources {
            if !existing.sources.contains(&source) {
                existing.sources.push(source);
            }
        }
    } else {
        subtitles.push(subtitle);
    }
}

fn push_warning(warnings: &mut Vec<String>, warning: String) {
    if !warnings.contains(&warning) {
        warnings.push(warning);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use map::MapRecord;
    use std::collections::HashSet;

    pub(super) fn test_maps(
        records: HashMap<u32, HashSet<MapRecord>>,
        banks: &[super::super::Bank],
    ) -> Vec<SoundMap<'_>> {
        vec![SoundMap {
            path: "ENGLISH/MAPS.LM0".to_owned(),
            records,
            banks: banks.iter().collect(),
        }]
    }

    pub(super) fn test_bank(offsets: &[u64]) -> Vec<super::super::Bank> {
        vec![super::super::Bank {
            name: "1_1_2.LS0".to_owned(),
            map: None,
            sound_kind: None,
            path: "ENGLISH/1_1_2.LS0".to_owned(),
            code: "1_1_2".to_owned(),
            mission_name: Some("Police Station".to_owned()),
            size: 0,
            issue: None,
            segments: offsets
                .iter()
                .enumerate()
                .map(|(index, &offset)| super::super::Segment {
                    id: format!("ENGLISH/1_1_2.LS0:{offset}"),
                    source: super::super::SegmentSource::Stream {
                        offset,
                        byte_length: 10,
                    },
                    name: None,
                    sample_count: 20,
                    sample_rate: 36000,
                    channels: 1,
                    codec: "Ubisoft ADPCM 4-bit".to_owned(),
                    segment: index,
                })
                .collect(),
        }]
    }

    /// Records of the given keys' audio in [`test_bank`]'s bank.
    pub(super) fn test_records(
        keys_and_offsets: &[(u32, u32)],
    ) -> HashMap<u32, HashSet<MapRecord>> {
        test_records_in("1_1_2.LS0", keys_and_offsets)
    }

    pub(super) fn test_records_in(
        bank: &str,
        keys_and_offsets: &[(u32, u32)],
    ) -> HashMap<u32, HashSet<MapRecord>> {
        keys_and_offsets
            .iter()
            .map(|&(key, offset)| {
                (
                    key,
                    HashSet::from([MapRecord {
                        resource_key: key,
                        offset,
                        compressed_bytes: 10,
                        sample_count: 20,
                        sample_rate: 36000,
                        bank: bank.to_owned(),
                    }]),
                )
            })
            .collect()
    }

    #[test]
    fn rejects_unknown_languages_before_running_uelib() {
        let mut cache = GameCache::default();
        let error = load_verified_subtitles(
            Path::new("missing-game"),
            "KLINGON",
            &[],
            &mut cache,
            &AtomicBool::new(false),
        )
        .unwrap_err();
        assert_eq!(
            error.to_string(),
            "Subtitle localization extension is not known for language KLINGON."
        );
        assert!(!cache.has_read_scripts());
    }

    #[test]
    fn stops_before_running_uelib_when_cancelled() {
        let game = tempfile::tempdir().unwrap();
        let english = game.path().join("Sounds/ENGLISH");
        fs::create_dir_all(&english).unwrap();
        fs::write(english.join("MAPS.LM0"), []).unwrap();
        let mut cache = GameCache::default();

        let error = load_verified_subtitles(
            game.path(),
            "ENGLISH",
            &[],
            &mut cache,
            &AtomicBool::new(true),
        )
        .unwrap_err();
        assert_eq!(error, Error::Cancelled);
        assert!(!cache.has_read_scripts());
    }

    #[cfg(feature = "uelib-bridge")]
    #[test]
    fn reports_the_bridge_error_message() {
        let game = tempfile::tempdir().unwrap();
        let english = game.path().join("Sounds/ENGLISH");
        fs::create_dir_all(&english).unwrap();
        fs::write(english.join("MAPS.LM0"), []).unwrap();

        let error = load_verified_subtitles(
            game.path(),
            "ENGLISH",
            &[],
            &mut GameCache::default(),
            &AtomicBool::new(false),
        )
        .unwrap_err();
        let message = error.to_string();
        assert!(
            message.starts_with("Game system directory was not found: "),
            "{message}"
        );
    }
}
