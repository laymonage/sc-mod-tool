mod alignment;
mod bridge;
mod localization;
pub(crate) mod map;

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use encoding_rs::WINDOWS_1252;

use crate::sound_keys::{self, KeyCorrections, PackageKeys};
use crate::{Error, find_file};
use alignment::match_unshown_subtitles;
use bridge::{BridgeLink, BridgePattern, scan_compiled_links};
use localization::{find_cue, line_text, localization_path, localization_sections};
use map::{SoundMap, mark_foreign_packages, parse_map_records, resolve_segments};

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

#[derive(Clone, Debug, Default)]
pub struct SubtitleAnalysis {
    pub by_segment: HashMap<String, Vec<Subtitle>>,
    pub warnings: Vec<String>,
    pub explicit_pairs: usize,
    pub mapped_pairs: usize,
    /// Sounds matched to text the game doesn't show, by section order.
    pub unshown_pairs: usize,
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
/// loading subtitles, so callers can keep them between languages of a game.
#[derive(Debug)]
pub struct CompiledLinks {
    links: Vec<BridgeLink>,
    issues: Vec<String>,
    patterns: Vec<BridgePattern>,
    corrections: KeyCorrections,
    package_keys: Vec<PackageKeys>,
}

/// Matches `language`'s sound banks to subtitle cues.
///
/// `compiled_links` caches the UELib analysis for the game at `root`. Pass
/// `None` to run it (it is then filled in), or a value from an earlier call for
/// the same game root to reuse it.
///
/// Stops with [`Error::Cancelled`] once `cancelled` is set. UELib's analysis
/// can't be interrupted, so cancelling while it runs takes effect when it
/// finishes. Its result is still kept in `compiled_links`.
pub fn load_verified_subtitles(
    root: &Path,
    language: &str,
    banks: &[super::Bank],
    compiled_links: &mut Option<Arc<CompiledLinks>>,
    cancelled: &AtomicBool,
) -> Result<SubtitleAnalysis, Error> {
    if cancelled.load(Ordering::Relaxed) {
        return Err(Error::Cancelled);
    }
    let extension = super::known_language(language)
        .ok_or_else(|| {
            format!("Subtitle localization extension is not known for language {language}.")
        })?
        .localization_extension;

    let sounds = super::find_sounds_directory(root)?;
    let game_root = sounds.parent().ok_or_else(|| {
        format!(
            "Could not find the game directory above {}.",
            sounds.display()
        )
    })?;
    let mut folders = vec![super::LanguageFolder {
        map: None,
        relative_path: language.to_owned(),
    }];
    folders.extend(super::map_language_folders(&sounds, language)?);
    let mut maps = Vec::with_capacity(folders.len());
    for folder in folders {
        let map_path = find_file(
            &super::safe_child_path(&sounds, &folder.relative_path)?,
            "MAPS.LM0",
        )?;
        let map_data = fs::read(&map_path)
            .map_err(|error| format!("Could not read {}: {error}", map_path.display()))?;
        maps.push(SoundMap {
            records: parse_map_records(&map_data),
            banks: banks.iter().filter(|bank| bank.map == folder.map).collect(),
            folder: folder.relative_path,
            foreign_packages: HashSet::new(),
        });
    }

    let bridge = match compiled_links {
        Some(links) => Arc::clone(links),
        None => {
            let result = scan_compiled_links(game_root, cancelled)?;
            let played = result
                .links
                .iter()
                .map(|link| &link.sound_event)
                .chain(
                    result
                        .patterns
                        .iter()
                        .flat_map(|pattern| &pattern.calls)
                        .map(|call| &call.sound_event),
                )
                .map(|sound_event| sound_event.to_ascii_lowercase())
                .collect();
            let corrections =
                sound_keys::correct(game_root, &sounds, &result.sound_packages, &played);
            let package_keys = sound_keys::package_keys(&result.sound_packages, &corrections);
            compiled_links
                .insert(Arc::new(CompiledLinks {
                    links: result.links,
                    issues: result.issues,
                    patterns: result.patterns,
                    corrections,
                    package_keys,
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
    for note in &bridge.corrections.notes {
        push_warning(&mut analysis.warnings, note.clone());
    }
    for map in &mut maps {
        mark_foreign_packages(map, &bridge.package_keys, &mut analysis.warnings);
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
        let key = bridge
            .corrections
            .key(&link.sound_event)
            .unwrap_or(link.sound_map_key);
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
                    &bridge.corrections,
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

    pub(super) fn test_maps(
        records: HashMap<u32, HashSet<MapRecord>>,
        banks: &[super::super::Bank],
    ) -> Vec<SoundMap<'_>> {
        vec![SoundMap {
            folder: "ENGLISH".to_owned(),
            records,
            banks: banks.iter().collect(),
            foreign_packages: HashSet::new(),
        }]
    }

    pub(super) fn test_bank(offsets: &[u64]) -> Vec<super::super::Bank> {
        vec![super::super::Bank {
            name: "1_1_2.LS0".to_owned(),
            map: None,
            path: "ENGLISH/1_1_2.LS0".to_owned(),
            code: "1_1_2".to_owned(),
            size: 0,
            issue: None,
            segments: offsets
                .iter()
                .enumerate()
                .map(|(index, &offset)| super::super::Segment {
                    id: format!("ENGLISH/1_1_2.LS0:{offset}"),
                    language: "ENGLISH".to_owned(),
                    bank: "1_1_2.LS0".to_owned(),
                    code: "1_1_2".to_owned(),
                    offset,
                    byte_length: 10,
                    sample_count: 20,
                    sample_rate: 36000,
                    channels: 1,
                    codec: "Ubisoft ADPCM 4-bit",
                    segment: index,
                })
                .collect(),
        }]
    }

    pub(super) fn test_records(
        keys_and_offsets: &[(u32, u32)],
    ) -> HashMap<u32, HashSet<MapRecord>> {
        keys_and_offsets
            .iter()
            .map(|&(key, offset)| {
                (
                    key,
                    HashSet::from([MapRecord {
                        offset,
                        compressed_bytes: 10,
                        sample_count: 20,
                        sample_rate: 36000,
                        bank: None,
                    }]),
                )
            })
            .collect()
    }

    #[test]
    fn rejects_unknown_languages_before_running_uelib() {
        let mut compiled_links = None;
        let error = load_verified_subtitles(
            Path::new("missing-game"),
            "KLINGON",
            &[],
            &mut compiled_links,
            &AtomicBool::new(false),
        )
        .unwrap_err();
        assert_eq!(
            error.to_string(),
            "Subtitle localization extension is not known for language KLINGON."
        );
        assert!(compiled_links.is_none());
    }

    #[test]
    fn stops_before_running_uelib_when_cancelled() {
        let game = tempfile::tempdir().unwrap();
        let english = game.path().join("Sounds/ENGLISH");
        fs::create_dir_all(&english).unwrap();
        fs::write(english.join("MAPS.LM0"), []).unwrap();
        let mut compiled_links = None;

        let error = load_verified_subtitles(
            game.path(),
            "ENGLISH",
            &[],
            &mut compiled_links,
            &AtomicBool::new(true),
        )
        .unwrap_err();
        assert_eq!(error, Error::Cancelled);
        assert!(compiled_links.is_none());
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
            &mut None,
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
