use std::collections::{HashMap, HashSet};
#[cfg(feature = "uelib-bridge")]
use std::ffi::CString;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(feature = "uelib-bridge")]
use std::sync::{Mutex, PoisonError};

use encoding_rs::WINDOWS_1252;
use serde::Deserialize;

use crate::Error;
use crate::sound_keys::{self, KeyCorrections, PackageKeys};

const MAP_RECORD_SIZE: usize = 56;
#[cfg(feature = "uelib-bridge")]
const BRIDGE_OUTPUT_CAPACITY: usize = 16 * 1024 * 1024;

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

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BridgeLink {
    source_package: String,
    source_object: String,
    section: String,
    cue: String,
    localization_file: String,
    sound_event: String,
    sound_map_key: u32,
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

/// A script pattern's sound references, in script order.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BridgePattern {
    pattern: String,
    source_package: String,
    calls: Vec<BridgeCall>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BridgeCall {
    source_object: String,
    sound_event: String,
    #[serde(default)]
    sound_map_key: Option<u32>,
    /// Set when the sound is passed with a `Localize` cue.
    #[serde(default)]
    section: Option<String>,
    #[serde(default)]
    cue: Option<String>,
    /// The function the sound is passed to, such as `__NFUN_1408__`.
    #[serde(default)]
    function: Option<String>,
}

impl BridgeCall {
    /// Whether the call speaks a line of dialogue: a comms transmission
    /// (native 1407, with a `Localize` cue) or a conversation line (native
    /// 1408). Sounds played through an actor (native 264) are effects or
    /// background voices without lines of their own.
    fn is_dialogue(&self) -> bool {
        self.cue.is_some()
            || matches!(
                self.function.as_deref(),
                Some("__NFUN_1407__" | "__NFUN_1408__")
            )
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BridgeResult {
    // A failed analysis returns only `error`.
    #[serde(default)]
    links: Vec<BridgeLink>,
    #[serde(default)]
    issues: Vec<String>,
    #[serde(default)]
    sound_packages: Vec<sound_keys::SoundPackage>,
    #[serde(default)]
    patterns: Vec<BridgePattern>,
    #[cfg(feature = "uelib-bridge")]
    error: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct MapRecord {
    offset: u32,
    compressed_bytes: u32,
    pub(crate) sample_count: u32,
    /// Zero when the record is too short to include it.
    pub(crate) sample_rate: u32,
    /// The bank file holding the audio, such as `1_3_2.LS0`. A level can play
    /// sounds from another level's package, so this can't be inferred from the
    /// package name.
    bank: Option<String>,
}

/// A `MAPS.LM0` and the banks it describes: the language's own folder, or one
/// of an extra map's language folders, which have their own banks and maps.
struct SoundMap<'a> {
    /// The folder, relative to `Sounds`, such as `1_7_1_2Vselka/English`.
    folder: String,
    records: HashMap<u32, HashSet<MapRecord>>,
    banks: Vec<&'a super::Bank>,
    /// IDs of packages this map was made for another version of, whose keys
    /// point at other audio here.
    foreign_packages: HashSet<u32>,
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
    if language == "." {
        return Ok(SubtitleAnalysis::default());
    }
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
            let (text, _, had_errors) = WINDOWS_1252.decode(&bytes);
            if had_errors {
                push_warning(
                    &mut analysis.warnings,
                    format!(
                        "Could not decode localization file as Windows-1252: {}",
                        localization_path.display()
                    ),
                );
                continue;
            }
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

/// Finds the packages `map` was made for another version of: most of its
/// records under the package's ID are for sounds the package doesn't have. The
/// extra Vselka maps were made this way for `S3_1_1Voice`, so the game's keys
/// for that package point at other lines there.
fn mark_foreign_packages(map: &mut SoundMap, packages: &[PackageKeys], warnings: &mut Vec<String>) {
    for package in packages {
        let (mut known, mut unknown) = (0, 0);
        for key in map.records.keys().filter(|key| *key >> 16 == package.id) {
            if package.keys.contains(key) {
                known += 1;
            } else {
                unknown += 1;
            }
        }
        if unknown > known {
            map.foreign_packages.insert(package.id);
            push_warning(
                warnings,
                format!(
                    "{}/MAPS.LM0 was made for another version of {}: {unknown} of its {} sounds for that package aren't in {}.uax, so the package's subtitles aren't matched there.",
                    map.folder,
                    package.package,
                    known + unknown,
                    package.package
                ),
            );
        }
    }
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

/// Matches the sounds a pattern plays without a subtitle to the lines of the
/// localization section named after the pattern, by position.
///
/// A pattern's lines of dialogue and its section's spoken lines are in the
/// same order. Other lines, such as `Note_` and `Goal_` text shown on screen,
/// aren't voiced and are skipped. The alignment is made against the English
/// section (`reference_sections`), and each line's text is then looked up in
/// this language by cue name: other languages' sections can keep lines that
/// English dropped, which would throw positions off. If English doesn't line
/// up, this language's section is aligned on its own.
fn match_unshown_subtitles(
    analysis: &mut SubtitleAnalysis,
    pattern: &BridgePattern,
    sections: &HashMap<String, Vec<(String, String)>>,
    reference_sections: &HashMap<String, Vec<(String, String)>>,
    corrections: &KeyCorrections,
    maps: &[SoundMap],
) {
    let lines = spoken_lines(sections, &pattern.pattern);
    let reference_lines = spoken_lines(reference_sections, &pattern.pattern);
    let calls = pattern
        .calls
        .iter()
        .filter(|call| call.is_dialogue())
        .collect::<Vec<_>>();
    // Only scenes that speak lines without a subtitle are of interest.
    if lines.is_empty() || calls.iter().all(|call| call.cue.is_some()) {
        return;
    }
    let key_of = |call: &BridgeCall| corrections.key(&call.sound_event).or(call.sound_map_key);
    let durations = calls
        .iter()
        .map(|call| key_of(call).and_then(|key| clip_seconds(key, maps)))
        .collect::<Vec<_>>();

    let matched = match align(&pattern.pattern, &calls, &reference_lines, &durations) {
        Ok(alignment) => alignment
            .iter()
            .map(|&line| cue_in_language(&reference_lines, line, &lines))
            .collect::<Vec<_>>(),
        Err(_) => match align(&pattern.pattern, &calls, &lines, &durations) {
            Ok(alignment) => alignment.iter().map(|&line| Some(lines[line])).collect(),
            Err(warning) => {
                push_warning(&mut analysis.warnings, warning);
                return;
            }
        },
    };

    for (call, line) in calls.iter().zip(matched) {
        let Some((cue, text)) = line else {
            continue;
        };
        if call.cue.is_some() {
            continue;
        }
        let Some(key) = key_of(call) else {
            continue;
        };
        let segments = match resolve_segments(&call.sound_event, key, maps) {
            Ok(segments) => segments,
            Err(warning) => {
                push_warning(&mut analysis.warnings, warning);
                continue;
            }
        };
        for segment in segments {
            add_subtitle(
                analysis,
                &segment.id,
                Subtitle {
                    cue: cue.clone(),
                    text: line_text(text),
                    sources: vec![format!(
                        "{}: {}",
                        pattern.source_package, call.source_object
                    )],
                    sound_event: call.sound_event.clone(),
                    sound_map_key: key,
                    confidence: MappingConfidence::SectionOrder,
                },
            );
        }
        analysis.unshown_pairs += 1;
    }
}

/// A section's spoken lines, in file order. `Note_` and `Goal_` lines are text
/// shown on screen and aren't voiced.
fn spoken_lines<'a>(
    sections: &'a HashMap<String, Vec<(String, String)>>,
    section: &str,
) -> Vec<&'a (String, String)> {
    sections
        .get(&section.to_ascii_lowercase())
        .map(|lines| {
            lines
                .iter()
                .filter(|(key, _)| key.to_ascii_lowercase().starts_with("speech_"))
                .collect()
        })
        .unwrap_or_default()
}

/// Lines a scene's dialogue calls up with a section's spoken lines, returning
/// each call's line, or a warning explaining why they don't line up.
fn align(
    pattern: &str,
    calls: &[&BridgeCall],
    lines: &[&(String, String)],
    durations: &[Option<f64>],
) -> Result<Vec<usize>, String> {
    if lines.is_empty() {
        return Err(format!(
            "{pattern} has no spoken lines in its localization section, so its unshown subtitles weren't matched."
        ));
    }
    if lines.len() == calls.len() {
        let alignment = (0..calls.len()).collect::<Vec<_>>();
        return if is_anchored(pattern, calls, lines, &alignment) {
            Ok(alignment)
        } else {
            Err(format!(
                "{pattern}'s subtitled sounds don't line up with its localization section, so its unshown subtitles weren't matched."
            ))
        };
    }
    if lines.len() > calls.len() {
        return skipping_alignment(pattern, calls, lines, durations).ok_or_else(|| {
            format!(
                "{pattern} speaks {} lines but its localization section has {}, and their lengths don't show which lines it skips, so its unshown subtitles weren't matched.",
                calls.len(),
                lines.len()
            )
        });
    }
    Err(format!(
        "{pattern} speaks {} lines but its localization section has only {}, so its unshown subtitles weren't matched.",
        calls.len(),
        lines.len()
    ))
}

/// A localization line's text, or `None` for a line without text: the game's
/// files use `(null)` for those.
fn line_text(text: &str) -> Option<String> {
    let text = text.trim();
    (!text.is_empty() && !text.eq_ignore_ascii_case("(null)")).then(|| text.to_owned())
}

/// Finds the line of `lines` with the same cue as `reference_lines[index]`.
/// Cue names can repeat within a section, so the same occurrence is used.
fn cue_in_language<'a>(
    reference_lines: &[&(String, String)],
    index: usize,
    lines: &[&'a (String, String)],
) -> Option<&'a (String, String)> {
    let cue = &reference_lines[index].0;
    let occurrence = reference_lines[..index]
        .iter()
        .filter(|(key, _)| key.eq_ignore_ascii_case(cue))
        .count();
    lines
        .iter()
        .filter(|(key, _)| key.eq_ignore_ascii_case(cue))
        .nth(occurrence)
        .copied()
}

/// Whether every call with a `Localize` cue sits at that cue's line, where
/// `alignment` gives each call's line.
fn is_anchored(
    pattern: &str,
    calls: &[&BridgeCall],
    lines: &[&(String, String)],
    alignment: &[usize],
) -> bool {
    calls
        .iter()
        .zip(alignment)
        .all(|(call, &line)| match (&call.section, &call.cue) {
            (Some(section), Some(cue)) => {
                section.eq_ignore_ascii_case(pattern) && cue.eq_ignore_ascii_case(&lines[line].0)
            }
            _ => true,
        })
}

/// The most ways of skipping a section's extra lines that are compared.
const MAX_SKIP_ALIGNMENTS: usize = 200;

/// When a section has lines its scene never speaks, finds which ones to skip.
///
/// Each way of skipping the extra lines is scored by how much the speaking
/// rate (characters per second of clip) varies across the lines it keeps. The
/// most even alignment is used only when it clearly beats every other.
fn skipping_alignment(
    pattern: &str,
    calls: &[&BridgeCall],
    lines: &[&(String, String)],
    durations: &[Option<f64>],
) -> Option<Vec<usize>> {
    let mut scored = Vec::new();
    for skipped in combinations(lines.len(), lines.len() - calls.len(), MAX_SKIP_ALIGNMENTS)? {
        let alignment = (0..lines.len())
            .filter(|line| !skipped.contains(line))
            .collect::<Vec<_>>();
        if !is_anchored(pattern, calls, lines, &alignment) {
            continue;
        }
        let log_rates = alignment
            .iter()
            .zip(durations)
            .filter_map(|(&line, duration)| {
                Some((spoken_length(&lines[line].1) as f64 / (*duration)?).ln())
            })
            .collect::<Vec<_>>();
        if log_rates.len() < 2 {
            continue;
        }
        let mean = log_rates.iter().sum::<f64>() / log_rates.len() as f64;
        let spread = log_rates
            .iter()
            .map(|rate| (rate - mean).powi(2))
            .sum::<f64>();
        scored.push((spread, alignment));
    }
    scored.sort_by(|left, right| left.0.total_cmp(&right.0));
    match scored.as_slice() {
        [(_, best)] => Some(best.clone()),
        [(best, alignment), (second, _), ..] => (best * 2.0 < *second).then(|| alignment.clone()),
        [] => None,
    }
}

/// Every way of choosing `count` of `0..total`, or `None` if there are more
/// than `limit`.
fn combinations(total: usize, count: usize, limit: usize) -> Option<Vec<Vec<usize>>> {
    let mut size = 1usize;
    for index in 0..count.min(total - count) {
        size = size.checked_mul(total - index)? / (index + 1);
        if size > limit {
            return None;
        }
    }
    let mut all = Vec::with_capacity(size);
    let mut current = (0..count).collect::<Vec<_>>();
    loop {
        all.push(current.clone());
        let Some(position) = (0..count)
            .rev()
            .find(|&position| current[position] < total - count + position)
        else {
            return Some(all);
        };
        current[position] += 1;
        for next in position + 1..count {
            current[next] = current[next - 1] + 1;
        }
    }
}

/// Counts the characters spoken in a line, leaving out a leading speaker name
/// such as `SOLDIER A - ` and stage directions in parentheses.
fn spoken_length(text: &str) -> usize {
    let text = match text.split_once(" - ") {
        Some((speaker, rest))
            if !speaker.trim().is_empty()
                && speaker.trim().chars().all(|character| {
                    character.is_ascii_uppercase() || character.is_ascii_digit() || character == ' '
                }) =>
        {
            rest
        }
        _ => text,
    };
    let mut depth = 0usize;
    text.chars()
        .filter(|&character| {
            match character {
                '(' => depth += 1,
                ')' => depth = depth.saturating_sub(1),
                _ => return depth == 0 && !character.is_whitespace(),
            }
            false
        })
        .count()
}

/// The duration of the clip a key's map record describes, from the first map
/// that has the key.
fn clip_seconds(key: u32, maps: &[SoundMap]) -> Option<f64> {
    let record = maps
        .iter()
        .filter(|map| !map.foreign_packages.contains(&(key >> 16)))
        .find_map(|map| map.records.get(&key))?
        .iter()
        .next()?;
    let rate = if record.sample_rate == 0 {
        36000
    } else {
        record.sample_rate
    };
    Some(f64::from(record.sample_count) / f64::from(rate))
}

/// Reads every section of the localization files with `extension`, keeping
/// each section's lines in file order. Cue names can repeat within a section.
fn localization_sections(
    localization_root: &Path,
    extension: &str,
) -> Result<HashMap<String, Vec<(String, String)>>, String> {
    let entries = fs::read_dir(localization_root).map_err(|error| {
        format!(
            "Could not read directory {}: {error}",
            localization_root.display()
        )
    })?;
    let mut sections = HashMap::<String, Vec<(String, String)>>::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path
            .extension()
            .is_some_and(|value| value.eq_ignore_ascii_case(extension))
        {
            continue;
        }
        let Ok(bytes) = fs::read(&path) else {
            continue;
        };
        let (text, _, _) = WINDOWS_1252.decode(&bytes);
        parse_sections(&text, &mut sections);
    }
    Ok(sections)
}

fn parse_sections(text: &str, sections: &mut HashMap<String, Vec<(String, String)>>) {
    let mut current: Option<String> = None;
    for line in text.lines().map(str::trim) {
        if let Some(name) = line
            .strip_prefix('[')
            .and_then(|line| line.strip_suffix(']'))
        {
            let name = name.trim().to_ascii_lowercase();
            // A section that appears in several files keeps its first copy.
            current = (!sections.contains_key(&name)).then(|| {
                sections.insert(name.clone(), Vec::new());
                name
            });
            continue;
        }
        if let (Some(section), Some((key, value))) = (&current, line.split_once('=')) {
            sections
                .get_mut(section)
                .expect("current section was inserted")
                .push((key.trim().to_owned(), value.trim().to_owned()));
        }
    }
}

fn scan_compiled_links(game_root: &Path, cancelled: &AtomicBool) -> Result<BridgeResult, Error> {
    #[cfg(feature = "uelib-bridge")]
    {
        unsafe extern "C" {
            fn scmod_scan_script_audio_links(
                game_root_utf8: *const std::ffi::c_char,
                output_utf8: *mut u8,
                output_capacity: i32,
            ) -> i32;
        }

        let game_root = game_root
            .to_str()
            .ok_or_else(|| format!("Game path is not valid UTF-8: {}", game_root.display()))?;
        let game_root = CString::new(game_root)
            .map_err(|_| "Game path contains an unsupported null character.".to_owned())?;
        // UELib's decompiler keeps its state in static fields, so concurrent
        // analyses would corrupt each other's output. Run one at a time.
        static BRIDGE_LOCK: Mutex<()> = Mutex::new(());
        let _bridge_guard = BRIDGE_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
        // Another analysis may have held the lock for a long time.
        if cancelled.load(Ordering::Relaxed) {
            return Err(Error::Cancelled);
        }
        let mut output = vec![0u8; BRIDGE_OUTPUT_CAPACITY];
        let status = unsafe {
            scmod_scan_script_audio_links(
                game_root.as_ptr(),
                output.as_mut_ptr(),
                i32::try_from(output.len()).expect("bridge output capacity fits i32"),
            )
        };
        if status == -2 {
            return Err(format!(
                "UELib analysis output exceeded the {}-byte bridge buffer.",
                output.len()
            )
            .into());
        }

        let output_length = output
            .iter()
            .position(|byte| *byte == 0)
            .unwrap_or(output.len());
        let output = std::str::from_utf8(&output[..output_length])
            .map_err(|error| format!("UELib returned invalid UTF-8: {error}"))?;
        let result: BridgeResult = serde_json::from_str(output)
            .map_err(|error| format!("Could not parse UELib analysis output: {error}"))?;
        if status != 0 {
            return Err(result
                .error
                .unwrap_or_else(|| format!("UELib analysis failed with status {status}."))
                .into());
        }
        Ok(result)
    }

    #[cfg(not(feature = "uelib-bridge"))]
    {
        let _ = (game_root, cancelled);
        Err("This build does not include the UELib bridge needed for subtitle matching.".into())
    }
}

pub(crate) fn parse_map_records(data: &[u8]) -> HashMap<u32, HashSet<MapRecord>> {
    let mut records = HashMap::<u32, HashSet<MapRecord>>::new();
    if data.len() < MAP_RECORD_SIZE {
        return records;
    }

    for position in (0..=data.len() - MAP_RECORD_SIZE).step_by(4) {
        let Some(key) = read_u32(data, position) else {
            continue;
        };
        if read_u32(data, position + 4) != Some(1) || read_u32(data, position + 8) != Some(1) {
            continue;
        }

        let Some(compressed_bytes) = read_u32(data, position + 12) else {
            continue;
        };
        let Some(offset) = read_u32(data, position + 20) else {
            continue;
        };
        let Some(sample_count) = read_u32(data, position + 48) else {
            continue;
        };
        if compressed_bytes == 0
            || sample_count == 0
            || read_u32(data, position + 52) != Some(compressed_bytes)
        {
            continue;
        }

        records.entry(key).or_default().insert(MapRecord {
            offset,
            compressed_bytes,
            sample_count,
            sample_rate: read_u32(data, position + 68).unwrap_or(0),
            bank: read_bank_name(data, position + 80),
        });
    }
    records
}

/// Reads a record's null-terminated bank file name, which is at most 32 bytes.
fn read_bank_name(data: &[u8], position: usize) -> Option<String> {
    let field = data.get(position..position.checked_add(32)?)?;
    let name = &field[..field.iter().position(|byte| *byte == 0)?];
    (!name.is_empty() && name.is_ascii()).then(|| String::from_utf8_lossy(name).into_owned())
}

fn read_u32(data: &[u8], position: usize) -> Option<u32> {
    let bytes = data.get(position..position.checked_add(4)?)?;
    Some(u32::from_le_bytes(bytes.try_into().ok()?))
}

/// Finds a sound's segment in every map that has it. Extra maps ship their own
/// copies of some banks, so a sound can have a segment in each.
fn resolve_segments<'a>(
    sound_event: &str,
    key: u32,
    maps: &[SoundMap<'a>],
) -> Result<Vec<&'a super::Segment>, String> {
    let mut segments = Vec::new();
    let mut warning = None;
    let mut in_foreign_map = false;
    for map in maps {
        if !map.records.contains_key(&key) {
            continue;
        }
        if map.foreign_packages.contains(&(key >> 16)) {
            in_foreign_map = true;
            continue;
        }
        match resolve_segment(sound_event, key, &map.records, &map.banks) {
            Ok(segment) => segments.push(segment),
            Err(error) => warning = warning.or(Some(error)),
        }
    }
    if segments.is_empty() {
        Err(warning.unwrap_or_else(|| {
            if in_foreign_map {
                format!(
                    "{sound_event} (key 0x{key:08X}) is only in maps made for another version of its package."
                )
            } else {
                format!("{sound_event} (key 0x{key:08X}) has no matching entry in MAPS.LM0.")
            }
        }))
    } else {
        Ok(segments)
    }
}

fn resolve_segment<'a>(
    sound_event: &str,
    key: u32,
    map_records: &HashMap<u32, HashSet<MapRecord>>,
    banks: &[&'a super::Bank],
) -> Result<&'a super::Segment, String> {
    let Some(records) = map_records.get(&key) else {
        return Err(format!(
            "{sound_event} (key 0x{key:08X}) has no matching entry in MAPS.LM0."
        ));
    };

    let expected_code = sound_package_bank_code(sound_event);
    let mut matches = Vec::new();
    for record in records {
        // Records name their bank; the package name is only a fallback.
        for bank in banks.iter().filter(|bank| match &record.bank {
            Some(name) => bank.name.eq_ignore_ascii_case(name),
            None => expected_code
                .as_deref()
                .is_none_or(|code| bank.code.eq_ignore_ascii_case(code)),
        }) {
            for segment in &bank.segments {
                if segment.offset == u64::from(record.offset)
                    && segment.sample_count == record.sample_count
                    && segment.byte_length == u64::from(record.compressed_bytes)
                    && !matches
                        .iter()
                        .any(|matched: &&super::Segment| matched.id == segment.id)
                {
                    matches.push(segment);
                }
            }
        }
    }

    match matches.as_slice() {
        [segment] => Ok(segment),
        [] => Err(format!(
            "{sound_event} (key 0x{key:08X}) did not resolve to a recognized segment in MAPS.LM0."
        )),
        _ => Err(format!(
            "{sound_event} (key 0x{key:08X}) resolves to multiple audio segments."
        )),
    }
}

fn sound_package_bank_code(sound_event: &str) -> Option<String> {
    let (package, _) = sound_event.split_once('.')?;
    let code = package
        .strip_prefix('S')?
        .strip_suffix("Voice")?
        .trim_end_matches('_');
    if code.is_empty()
        || !code
            .chars()
            .all(|character| character.is_ascii_digit() || character == '_')
    {
        return None;
    }
    Some(code.to_owned())
}

fn localization_path(
    localization_root: &Path,
    base_name: &str,
    extension: &str,
) -> Result<PathBuf, String> {
    let file_name = format!("{base_name}.{extension}");
    let exact_path = localization_root.join(&file_name);
    if exact_path.is_file() {
        return Ok(exact_path);
    }

    find_file(localization_root, &file_name).map_err(|_| {
        format!(
            "Localization file {file_name} was not found in {}.",
            localization_root.display()
        )
    })
}

pub(crate) fn find_file(directory: &Path, file_name: &str) -> Result<PathBuf, String> {
    let entries = fs::read_dir(directory)
        .map_err(|error| format!("Could not read directory {}: {error}", directory.display()))?;
    for entry in entries {
        let entry =
            entry.map_err(|error| format!("Could not inspect {}: {error}", directory.display()))?;
        if entry
            .file_name()
            .to_string_lossy()
            .eq_ignore_ascii_case(file_name)
        {
            return Ok(entry.path());
        }
    }
    Err(format!(
        "File {file_name} was not found in {}.",
        directory.display()
    ))
}

fn push_warning(warnings: &mut Vec<String>, warning: String) {
    if !warnings.contains(&warning) {
        warnings.push(warning);
    }
}

fn find_cue(text: &str, section: &str, cue: &str) -> Result<String, String> {
    let mut active_section = "";
    let mut found = None;

    for line in text.lines().map(str::trim) {
        if let Some(section_name) = line
            .strip_prefix('[')
            .and_then(|line| line.strip_suffix(']'))
        {
            active_section = section_name.trim();
            continue;
        }
        if active_section.eq_ignore_ascii_case(section)
            && let Some((key, value)) = line.split_once('=')
            && key.trim().eq_ignore_ascii_case(cue)
        {
            if found.is_some() {
                return Err(format!(
                    "Localization cue {cue} occurs more than once in section {section}."
                ));
            }
            let value = value.trim();
            if value.is_empty() {
                return Err(format!(
                    "Localization cue {cue} is empty in section {section}."
                ));
            }
            found = Some(value.to_owned());
        }
    }

    found.ok_or_else(|| format!("Localization cue {cue} was not found in section {section}."))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_map_record_key_offset_size_and_samples() {
        let mut data = vec![0; 128];
        write_u32(&mut data, 0, 0x0084_0001);
        write_u32(&mut data, 4, 1);
        write_u32(&mut data, 8, 1);
        write_u32(&mut data, 12, 15_571);
        write_u32(&mut data, 20, 173_296);
        write_u32(&mut data, 48, 29_967);
        write_u32(&mut data, 52, 15_571);
        write_u32(&mut data, 68, 36_000);
        data[80..89].copy_from_slice(b"0_0_2.LS0");

        let records = parse_map_records(&data);
        assert_eq!(
            records.get(&0x0084_0001),
            Some(&HashSet::from([MapRecord {
                offset: 173_296,
                compressed_bytes: 15_571,
                sample_count: 29_967,
                sample_rate: 36_000,
                bank: Some("0_0_2.LS0".to_owned()),
            }]))
        );
    }

    #[test]
    fn ignores_malformed_or_unaligned_map_records() {
        let mut data = vec![0; MAP_RECORD_SIZE + 8];
        write_u32(&mut data, 1, 0x0084_0001);
        write_u32(&mut data, 5, 1);
        write_u32(&mut data, 9, 1);
        write_u32(&mut data, 13, 15_571);
        write_u32(&mut data, 21, 173_296);
        write_u32(&mut data, 49, 29_967);
        write_u32(&mut data, 53, 15_571);

        assert!(parse_map_records(&data).is_empty());
    }

    #[test]
    fn matches_sound_packages_to_numeric_bank_names() {
        assert_eq!(
            sound_package_bank_code("S1_1_0Voice.Play_11_04_02"),
            Some("1_1_0".to_owned())
        );
        assert_eq!(
            sound_package_bank_code("S1_1_Voice.Play_11_90_01"),
            Some("1_1".to_owned())
        );
        assert_eq!(sound_package_bank_code("FisherVoice.Play_Alarm"), None);
    }

    #[test]
    fn finds_cue_in_the_requested_localization_section() {
        let text = "[Other]\nSpeech_0001L=Wrong\n[Pilot]\nSpeech_0001L=Speaker - Custom line\n";

        assert_eq!(
            find_cue(text, "Pilot", "Speech_0001L").unwrap(),
            "Speaker - Custom line"
        );
    }

    #[test]
    fn rejects_duplicate_or_missing_cues() {
        let duplicate = "[Pilot]\nSpeech_0001L=First\nSpeech_0001L=Second\n";
        assert!(find_cue(duplicate, "Pilot", "Speech_0001L").is_err());
        assert!(find_cue("[Pilot]\n", "Pilot", "Speech_0001L").is_err());
    }

    #[test]
    fn reads_the_sound_packages_reported_by_the_bridge() {
        let result: BridgeResult = serde_json::from_str(
            r#"{
                "links": [],
                "issues": [],
                "soundPackages": [{
                    "package": "S0_0_2Voice",
                    "sounds": [{
                        "name": "Play_00_15_01",
                        "exportIndex": 40,
                        "soundMapKey": 3932201,
                        "lipSyncPath": "\\S0_0_2Voice\\00_15_01.bin"
                    }]
                }]
            }"#,
        )
        .unwrap();

        assert_eq!(result.sound_packages.len(), 1);
    }

    #[test]
    fn keeps_section_lines_in_file_order_with_repeated_cues() {
        let mut sections = HashMap::new();
        parse_sections(
            "[Talk]\nSpeech_0020L=First\nSpeech_0001L=Second\nSpeech_0028L=Third\nSpeech_0028L=Fourth\n[Other]\nA=B\n",
            &mut sections,
        );

        assert_eq!(
            sections["talk"],
            [
                ("Speech_0020L".to_owned(), "First".to_owned()),
                ("Speech_0001L".to_owned(), "Second".to_owned()),
                ("Speech_0028L".to_owned(), "Third".to_owned()),
                ("Speech_0028L".to_owned(), "Fourth".to_owned()),
            ]
        );
    }

    fn test_maps(
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

    fn test_bank(offsets: &[u64]) -> Vec<super::super::Bank> {
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

    fn test_records(keys_and_offsets: &[(u32, u32)]) -> HashMap<u32, HashSet<MapRecord>> {
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

    fn test_call(sound_event: &str, key: u32, cue: Option<&str>) -> BridgeCall {
        BridgeCall {
            source_object: "State'EchelonPattern.P_Talk.Pattern'".to_owned(),
            sound_event: sound_event.to_owned(),
            sound_map_key: Some(key),
            section: cue.map(|_| "P_Talk".to_owned()),
            cue: cue.map(str::to_owned),
            function: Some(
                if cue.is_some() {
                    "__NFUN_1407__"
                } else {
                    "__NFUN_1408__"
                }
                .to_owned(),
            ),
        }
    }

    #[test]
    fn skips_the_unspoken_line_whose_removal_evens_out_the_speaking_rate() {
        // The scene speaks the last three lines; "What's that?" is never played.
        let clips = [(1, 0, 2.21), (2, 100, 2.72), (3, 200, 1.54)];
        let mut banks = test_bank(&[0, 100, 200]);
        let mut records = HashMap::new();
        for (index, &(key, offset, seconds)) in clips.iter().enumerate() {
            let sample_count = (seconds * 36000.0) as u32;
            banks[0].segments[index].sample_count = sample_count;
            records.insert(
                key,
                HashSet::from([MapRecord {
                    offset,
                    compressed_bytes: 10,
                    sample_count,
                    sample_rate: 36000,
                    bank: None,
                }]),
            );
        }
        let pattern = BridgePattern {
            pattern: "P_Talk".to_owned(),
            source_package: "EchelonPattern.u".to_owned(),
            calls: vec![
                test_call("S1_1_2Voice.Play_A", 1, None),
                test_call("S1_1_2Voice.Play_B", 2, None),
                test_call("S1_1_2Voice.Play_C", 3, None),
            ],
        };
        let mut sections = HashMap::new();
        parse_sections(
            "[P_Talk]\nSpeech_0005L=SOLDIER A -  Whats that?\nSpeech_0001L=SOLDIER A -  I thought this was what made the pools drain.\nSpeech_0002L=SOLDIER B -  It is, I think.  Maybe it's malfunctioning.\nSpeech_0003L=SOLDIER A -  It doesn't effect us anyway.\n",
            &mut sections,
        );
        let mut analysis = SubtitleAnalysis::default();

        match_unshown_subtitles(
            &mut analysis,
            &pattern,
            &sections,
            &sections,
            &KeyCorrections::default(),
            &test_maps(records, &banks),
        );

        assert_eq!(analysis.unshown_pairs, 3);
        assert_eq!(
            analysis.by_segment["ENGLISH/1_1_2.LS0:0"][0].cue,
            "Speech_0001L"
        );
        assert_eq!(
            analysis.by_segment["ENGLISH/1_1_2.LS0:200"][0].cue,
            "Speech_0003L"
        );
    }

    #[test]
    fn aligns_against_english_and_ignores_leftover_lines_in_other_languages() {
        let banks = test_bank(&[0, 100]);
        let records = test_records(&[(1, 0), (2, 100)]);
        let pattern = BridgePattern {
            pattern: "P_Talk".to_owned(),
            source_package: "EchelonPattern.u".to_owned(),
            calls: vec![
                test_call("S1_1_2Voice.Play_A", 1, None),
                test_call("S1_1_2Voice.Play_B", 2, None),
            ],
        };
        let mut english = HashMap::new();
        parse_sections(
            "[P_Talk]\nSpeech_0001L=Damn.\nSpeech_0002L=Stop him.\n",
            &mut english,
        );
        let mut german = HashMap::new();
        parse_sections(
            "[P_Talk]\nSpeech_0001L=Verdammt.\nSpeech_0002L=Haltet ihn auf.\nSpeech_0015L=Ein alter Entwurf.\n",
            &mut german,
        );
        let mut analysis = SubtitleAnalysis::default();

        match_unshown_subtitles(
            &mut analysis,
            &pattern,
            &german,
            &english,
            &KeyCorrections::default(),
            &test_maps(records, &banks),
        );

        assert_eq!(analysis.unshown_pairs, 2);
        assert_eq!(
            analysis.by_segment["ENGLISH/1_1_2.LS0:0"][0]
                .text
                .as_deref(),
            Some("Verdammt.")
        );
        assert_eq!(
            analysis.by_segment["ENGLISH/1_1_2.LS0:100"][0]
                .text
                .as_deref(),
            Some("Haltet ihn auf.")
        );
    }

    #[test]
    fn keeps_lines_without_text_matched_to_their_audio() {
        let banks = test_bank(&[0, 100]);
        let records = test_records(&[(1, 0), (2, 100)]);
        let pattern = BridgePattern {
            pattern: "P_Talk".to_owned(),
            source_package: "EchelonPattern.u".to_owned(),
            calls: vec![
                test_call("S1_1_2Voice.Play_A", 1, None),
                test_call("S1_1_2Voice.Play_B", 2, None),
            ],
        };
        let mut sections = HashMap::new();
        parse_sections(
            "[P_Talk]\nSpeech_0001L=(null)\nSpeech_0002L=Hello.\n",
            &mut sections,
        );
        let mut analysis = SubtitleAnalysis::default();

        match_unshown_subtitles(
            &mut analysis,
            &pattern,
            &sections,
            &sections,
            &KeyCorrections::default(),
            &test_maps(records, &banks),
        );

        let empty = &analysis.by_segment["ENGLISH/1_1_2.LS0:0"][0];
        assert_eq!(
            (empty.cue.as_str(), empty.text.as_deref()),
            ("Speech_0001L", None)
        );
        assert_eq!(analysis.segments_with_text(), 1);
        assert_eq!(analysis.segments_with_empty_lines(), 1);
    }

    #[test]
    fn counts_only_spoken_characters() {
        assert_eq!(spoken_length("GSOLD -  (interrupting) We're going."), 11);
        assert_eq!(spoken_length("Um...Okay."), 10);
        assert_eq!(spoken_length("CIA Agent - 28469"), 14);
    }

    #[test]
    fn lists_every_combination_up_to_a_limit() {
        assert_eq!(
            combinations(4, 2, 10).unwrap(),
            [[0, 1], [0, 2], [0, 3], [1, 2], [1, 3], [2, 3]]
        );
        assert_eq!(combinations(3, 0, 10).unwrap(), [Vec::<usize>::new()]);
        assert!(combinations(40, 4, 200).is_none());
        assert_eq!(combinations(10, 9, 200).unwrap().len(), 10);
    }

    #[test]
    fn skips_sound_effects_when_aligning_dialogue() {
        let banks = test_bank(&[0, 100]);
        let records = test_records(&[(1, 0), (2, 100)]);
        let effect = BridgeCall {
            function: Some("__NFUN_264__".to_owned()),
            ..test_call("DestroyableObjet.Play_ExplosionBridge", 9, None)
        };
        let pattern = BridgePattern {
            pattern: "P_Talk".to_owned(),
            source_package: "EchelonPattern.u".to_owned(),
            calls: vec![
                test_call("S1_1_2Voice.Play_A", 1, None),
                effect,
                test_call("S1_1_2Voice.Play_B", 2, None),
            ],
        };
        let mut sections = HashMap::new();
        parse_sections(
            "[P_Talk]\nSpeech_0001L=What was that?\nSpeech_0002L=We're being attacked.\n",
            &mut sections,
        );
        let mut analysis = SubtitleAnalysis::default();

        match_unshown_subtitles(
            &mut analysis,
            &pattern,
            &sections,
            &sections,
            &KeyCorrections::default(),
            &test_maps(records, &banks),
        );

        assert_eq!(analysis.unshown_pairs, 2);
        assert_eq!(
            analysis.by_segment["ENGLISH/1_1_2.LS0:100"][0]
                .text
                .as_deref(),
            Some("We're being attacked.")
        );
    }

    #[test]
    fn finds_a_sound_in_each_map_with_its_own_banks() {
        // Both maps ship a different 3_1_1.LS0, and their records point at
        // different offsets for the same key.
        let map_bank = |map: &str, offsets: &[u64]| {
            let mut bank = test_bank(offsets).remove(0);
            bank.name = "3_1_1.LS0".to_owned();
            bank.code = "3_1_1".to_owned();
            bank.map = Some(map.to_owned());
            bank.path = format!("{map}/English/3_1_1.LS0");
            for segment in &mut bank.segments {
                segment.id = format!("{}:{}", bank.path, segment.offset);
            }
            bank
        };
        let banks = [
            map_bank("1_7_1_1VselkaInfiltration", &[0, 100]),
            map_bank("1_7_1_2Vselka", &[0, 100]),
        ];
        let map = |folder: &str, records, banks| SoundMap {
            folder: folder.to_owned(),
            records,
            banks,
            foreign_packages: HashSet::new(),
        };
        let maps = [
            map(
                "1_7_1_1VselkaInfiltration/English",
                test_records(&[(7, 0)]),
                vec![&banks[0]],
            ),
            map(
                "1_7_1_2Vselka/English",
                test_records(&[(7, 100)]),
                vec![&banks[1]],
            ),
            map("ENGLISH", HashMap::new(), Vec::new()),
        ];

        let segments = resolve_segments("S3_1_1Voice.Play_31_01_01", 7, &maps).unwrap();

        assert_eq!(
            segments
                .iter()
                .map(|segment| segment.id.as_str())
                .collect::<Vec<_>>(),
            [
                "1_7_1_1VselkaInfiltration/English/3_1_1.LS0:0",
                "1_7_1_2Vselka/English/3_1_1.LS0:100",
            ]
        );
        assert_eq!(
            resolve_segments("S3_1_1Voice.Play_31_01_01", 8, &maps).unwrap_err(),
            "S3_1_1Voice.Play_31_01_01 (key 0x00000008) has no matching entry in MAPS.LM0."
        );
    }

    #[test]
    fn skips_packages_a_map_was_made_for_another_version_of() {
        // The package has sounds 0x450000-0x450002. The extra map files three
        // sounds under its ID, two of which the package doesn't have, so its
        // key 0x450001 means some other line there.
        let banks = test_bank(&[0, 100, 200]);
        let package = PackageKeys {
            package: "S1_1_2Voice".to_owned(),
            id: 0x45,
            keys: HashSet::from([0x450000, 0x450001, 0x450002]),
        };
        let mut maps = vec![
            SoundMap {
                folder: "ENGLISH".to_owned(),
                records: test_records(&[(0x450000, 0), (0x450001, 100)]),
                banks: vec![&banks[0]],
                foreign_packages: HashSet::new(),
            },
            SoundMap {
                folder: "1_7_1_2Vselka/English".to_owned(),
                records: test_records(&[(0x450001, 100), (0x450070, 0), (0x450071, 200)]),
                banks: vec![&banks[0]],
                foreign_packages: HashSet::new(),
            },
        ];
        let mut warnings = Vec::new();

        for map in &mut maps {
            mark_foreign_packages(map, std::slice::from_ref(&package), &mut warnings);
        }

        assert!(maps[0].foreign_packages.is_empty());
        assert_eq!(maps[1].foreign_packages, HashSet::from([0x45]));
        assert_eq!(
            warnings,
            [
                "1_7_1_2Vselka/English/MAPS.LM0 was made for another version of S1_1_2Voice: 2 of its 3 sounds for that package aren't in S1_1_2Voice.uax, so the package's subtitles aren't matched there."
            ]
        );
        let segments = resolve_segments("S1_1_2Voice.Play_11_38_02", 0x450001, &maps).unwrap();
        assert_eq!(
            segments
                .iter()
                .map(|segment| segment.id.as_str())
                .collect::<Vec<_>>(),
            ["ENGLISH/1_1_2.LS0:100"]
        );
        maps.remove(0);
        assert_eq!(
            resolve_segments("S1_1_2Voice.Play_11_38_02", 0x450001, &maps).unwrap_err(),
            "S1_1_2Voice.Play_11_38_02 (key 0x00450001) is only in maps made for another version of its package."
        );
    }

    #[test]
    fn finds_audio_in_the_bank_its_map_record_names() {
        // Level 1_3_2 plays a sound from level 1_3_3's package.
        let banks = test_bank(&[0, 100]);
        let mut records = test_records(&[(7, 100)]);
        let record = records.remove(&7).unwrap().into_iter().next().unwrap();
        records.insert(
            7,
            HashSet::from([MapRecord {
                bank: Some("1_1_2.LS0".to_owned()),
                ..record
            }]),
        );

        let segment = resolve_segment(
            "S1_3_3Voice.Play_13_21_01",
            7,
            &records,
            &banks.iter().collect::<Vec<_>>(),
        )
        .unwrap();

        assert_eq!(segment.id, "ENGLISH/1_1_2.LS0:100");
    }

    #[test]
    fn matches_unshown_sounds_to_section_lines_by_position() {
        let banks = test_bank(&[0, 100, 200]);
        let records = test_records(&[(1, 0), (2, 100), (3, 200)]);
        let pattern = BridgePattern {
            pattern: "P_Talk".to_owned(),
            source_package: "EchelonPattern.u".to_owned(),
            calls: vec![
                test_call("S1_1_2Voice.Play_A", 1, None),
                test_call("S1_1_2Voice.Play_B", 2, Some("Speech_0005L")),
                test_call("S1_1_2Voice.Play_C", 3, None),
            ],
        };
        let mut sections = HashMap::new();
        parse_sections(
            "[P_Talk]\nSpeech_0009L=Hello.\nSpeech_0005L=Shown.\nSpeech_0001L=Goodbye.\n",
            &mut sections,
        );
        let mut analysis = SubtitleAnalysis::default();

        match_unshown_subtitles(
            &mut analysis,
            &pattern,
            &sections,
            &sections,
            &KeyCorrections::default(),
            &test_maps(records, &banks),
        );

        assert_eq!(analysis.unshown_pairs, 2);
        let first = &analysis.by_segment["ENGLISH/1_1_2.LS0:0"][0];
        assert_eq!(
            (first.cue.as_str(), first.text.as_deref()),
            ("Speech_0009L", Some("Hello."))
        );
        assert_eq!(first.confidence, MappingConfidence::SectionOrder);
        assert_eq!(
            analysis.by_segment["ENGLISH/1_1_2.LS0:200"][0]
                .text
                .as_deref(),
            Some("Goodbye.")
        );
        // The sound passed with its cue is matched explicitly elsewhere.
        assert!(!analysis.by_segment.contains_key("ENGLISH/1_1_2.LS0:100"));
    }

    #[test]
    fn skips_section_lines_that_arent_spoken() {
        let banks = test_bank(&[0, 100]);
        let records = test_records(&[(1, 0), (2, 100)]);
        let pattern = BridgePattern {
            pattern: "P_Talk".to_owned(),
            source_package: "EchelonPattern.u".to_owned(),
            calls: vec![
                test_call("S1_1_2Voice.Play_A", 1, None),
                test_call("S1_1_2Voice.Play_B", 2, None),
            ],
        };
        let mut sections = HashMap::new();
        parse_sections(
            "[P_Talk]\nSpeech_0001L=What's the code?\nNote_0002L=The code is 28469.\nSpeech_0003L=28469.\n",
            &mut sections,
        );
        let mut analysis = SubtitleAnalysis::default();

        match_unshown_subtitles(
            &mut analysis,
            &pattern,
            &sections,
            &sections,
            &KeyCorrections::default(),
            &test_maps(records, &banks),
        );

        assert_eq!(analysis.unshown_pairs, 2);
        assert_eq!(
            analysis.by_segment["ENGLISH/1_1_2.LS0:100"][0]
                .text
                .as_deref(),
            Some("28469.")
        );
    }

    #[test]
    fn skips_patterns_whose_lines_dont_line_up() {
        let banks = test_bank(&[0, 100]);
        let maps = test_maps(test_records(&[(1, 0), (2, 100)]), &banks);
        let mut sections = HashMap::new();
        parse_sections(
            "[P_Talk]\nSpeech_0001L=One.\nSpeech_0002L=Two.\nSpeech_0003L=Six.\n",
            &mut sections,
        );
        let fewer_sounds = BridgePattern {
            pattern: "P_Talk".to_owned(),
            source_package: "EchelonPattern.u".to_owned(),
            calls: vec![
                test_call("S1_1_2Voice.Play_A", 1, None),
                test_call("S1_1_2Voice.Play_B", 2, None),
            ],
        };
        let misplaced_cue = BridgePattern {
            pattern: "P_Talk".to_owned(),
            source_package: "EchelonPattern.u".to_owned(),
            calls: vec![
                test_call("S1_1_2Voice.Play_A", 1, Some("Speech_0002L")),
                test_call("S1_1_2Voice.Play_B", 2, None),
                test_call("S1_1_2Voice.Play_C", 2, None),
            ],
        };
        let mut analysis = SubtitleAnalysis::default();

        for pattern in [&fewer_sounds, &misplaced_cue] {
            match_unshown_subtitles(
                &mut analysis,
                pattern,
                &sections,
                &sections,
                &KeyCorrections::default(),
                &maps,
            );
        }

        assert_eq!(analysis.unshown_pairs, 0);
        assert!(analysis.by_segment.is_empty());
        assert_eq!(analysis.warnings.len(), 2);
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

    fn write_u32(data: &mut [u8], position: usize, value: u32) {
        data[position..position + 4].copy_from_slice(&value.to_le_bytes());
    }
}
