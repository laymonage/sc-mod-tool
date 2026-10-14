use std::collections::{HashMap, HashSet};
#[cfg(feature = "uelib-bridge")]
use std::ffi::CString;
use std::fs;
use std::path::{Path, PathBuf};

use encoding_rs::WINDOWS_1252;
use serde::Deserialize;

use crate::Error;

const MAP_RECORD_SIZE: usize = 56;
const MAX_MAP_PADDING_BYTES: u64 = 2;
#[cfg(feature = "uelib-bridge")]
const BRIDGE_OUTPUT_CAPACITY: usize = 16 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MappingConfidence {
    ExplicitCompiledReference,
}

#[derive(Clone, Debug)]
pub struct Subtitle {
    pub cue: String,
    pub text: String,
    pub source: String,
    pub sound_event: String,
    pub sound_map_key: u32,
    pub confidence: MappingConfidence,
}

#[derive(Clone, Debug, Default)]
pub struct SubtitleAnalysis {
    pub by_segment: HashMap<String, Vec<Subtitle>>,
    pub warnings: Vec<String>,
    pub explicit_pairs: usize,
    pub mapped_pairs: usize,
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

#[derive(Debug, Deserialize)]
struct BridgeResult {
    links: Vec<BridgeLink>,
    issues: Vec<String>,
    #[cfg(feature = "uelib-bridge")]
    error: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct MapRecord {
    offset: u32,
    compressed_bytes: u32,
    sample_count: u32,
}

pub fn load_verified_subtitles(
    root: &Path,
    language: &str,
    banks: &[super::Bank],
) -> Result<SubtitleAnalysis, Error> {
    if language == "." {
        return Ok(SubtitleAnalysis::default());
    }

    let sounds = super::find_sounds_directory(root)?;
    let game_root = sounds.parent().ok_or_else(|| {
        format!(
            "Could not find the game directory above {}.",
            sounds.display()
        )
    })?;
    let language_root = super::safe_child_path(&sounds, language)?;
    let map_path = find_file(&language_root, "MAPS.LM0")?;
    let map_data = fs::read(&map_path)
        .map_err(|error| format!("Could not read {}: {error}", map_path.display()))?;
    let map_records = parse_map_records(&map_data);

    let bridge = scan_compiled_links(game_root)?;
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
    if bridge.links.is_empty() {
        push_warning(
            &mut analysis.warnings,
            "No direct compiled Localize/Sound references were found.".to_owned(),
        );
    }

    let localization_root = game_root.join("system").join("LOCALIZATION");
    let mut localization_cache = HashMap::<PathBuf, String>::new();
    for link in bridge.links {
        let segment = match resolve_segment(&link, &map_records, banks) {
            Ok(segment) => segment,
            Err(warning) => {
                push_warning(&mut analysis.warnings, warning);
                continue;
            }
        };

        let localization_path =
            match localization_path(&localization_root, &link.localization_file, language) {
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

        let source = format!("{}: {}", link.source_package, link.source_object);
        let subtitles = analysis.by_segment.entry(segment.id.clone()).or_default();
        if let Some(existing) = subtitles.iter_mut().find(|subtitle| {
            subtitle.cue.eq_ignore_ascii_case(&link.cue)
                && subtitle.sound_event.eq_ignore_ascii_case(&link.sound_event)
                && subtitle.sound_map_key == link.sound_map_key
        }) {
            if !existing.source.contains(&source) {
                existing.source.push_str("\n");
                existing.source.push_str(&source);
            }
        } else {
            subtitles.push(Subtitle {
                cue: link.cue,
                text,
                source,
                sound_event: link.sound_event,
                sound_map_key: link.sound_map_key,
                confidence: MappingConfidence::ExplicitCompiledReference,
            });
        }
        analysis.mapped_pairs += 1;
    }

    for subtitles in analysis.by_segment.values_mut() {
        subtitles.sort_by(|left, right| {
            left.cue
                .to_ascii_lowercase()
                .cmp(&right.cue.to_ascii_lowercase())
                .then_with(|| left.sound_event.cmp(&right.sound_event))
        });
    }
    analysis.warnings.sort();
    Ok(analysis)
}

fn scan_compiled_links(game_root: &Path) -> Result<BridgeResult, String> {
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
            ));
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
                .unwrap_or_else(|| format!("UELib analysis failed with status {status}.")));
        }
        Ok(result)
    }

    #[cfg(not(feature = "uelib-bridge"))]
    {
        let _ = game_root;
        Err("This build does not include the UELib bridge needed for subtitle matching.".to_owned())
    }
}

fn parse_map_records(data: &[u8]) -> HashMap<u32, HashSet<MapRecord>> {
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
        });
    }
    records
}

fn read_u32(data: &[u8], position: usize) -> Option<u32> {
    let bytes = data.get(position..position.checked_add(4)?)?;
    Some(u32::from_le_bytes(bytes.try_into().ok()?))
}

fn resolve_segment<'a>(
    link: &BridgeLink,
    map_records: &HashMap<u32, HashSet<MapRecord>>,
    banks: &'a [super::Bank],
) -> Result<&'a super::Segment, String> {
    let Some(records) = map_records.get(&link.sound_map_key) else {
        return Err(format!(
            "{} (key 0x{:08X}) has no matching entry in MAPS.LM0.",
            link.sound_event, link.sound_map_key
        ));
    };

    let expected_code = sound_package_bank_code(&link.sound_event);
    let mut matches = Vec::new();
    for record in records {
        for bank in banks.iter().filter(|bank| {
            expected_code
                .as_deref()
                .is_none_or(|code| bank.code.eq_ignore_ascii_case(code))
        }) {
            for segment in &bank.segments {
                if segment.offset == u64::from(record.offset)
                    && segment.sample_count == record.sample_count
                    && segment
                        .byte_length
                        .abs_diff(u64::from(record.compressed_bytes))
                        <= MAX_MAP_PADDING_BYTES
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
            "{} (key 0x{:08X}) did not resolve to a recognized segment in MAPS.LM0.",
            link.sound_event, link.sound_map_key
        )),
        _ => Err(format!(
            "{} (key 0x{:08X}) resolves to multiple audio segments.",
            link.sound_event, link.sound_map_key
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
    language: &str,
) -> Result<PathBuf, String> {
    let extension = match language.to_ascii_uppercase().as_str() {
        "ENGLISH" => "int",
        "GERMAN" => "deu",
        _ => {
            return Err(format!(
                "Subtitle localization extension is not known for language {language}."
            ));
        }
    };
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

fn find_file(directory: &Path, file_name: &str) -> Result<PathBuf, String> {
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
        let mut data = vec![0; MAP_RECORD_SIZE];
        write_u32(&mut data, 0, 0x0084_0001);
        write_u32(&mut data, 4, 1);
        write_u32(&mut data, 8, 1);
        write_u32(&mut data, 12, 15_571);
        write_u32(&mut data, 20, 173_296);
        write_u32(&mut data, 48, 29_967);
        write_u32(&mut data, 52, 15_571);

        let records = parse_map_records(&data);
        assert_eq!(
            records.get(&0x0084_0001),
            Some(&HashSet::from([MapRecord {
                offset: 173_296,
                compressed_bytes: 15_571,
                sample_count: 29_967,
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

    fn write_u32(data: &mut [u8], position: usize, value: u32) {
        data[position..position + 4].copy_from_slice(&value.to_le_bytes());
    }
}
