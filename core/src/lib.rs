pub mod decoder;
mod error;
mod sound_keys;
mod sound_maps;
pub mod subtitles;
mod vgmstream;

pub use error::Error;

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use encoding_rs::WINDOWS_1252;

use decoder::DecodedAudio;

const BANK_HEADER_SIZE: usize = 0x30;
const FRAME_HEADER_SIZE: usize = 0x34;
const CODES_PER_FRAME: u32 = 1536;

#[derive(Clone, Debug)]
pub struct Language {
    pub directory: String,
    pub label: String,
}

/// A numeric `.LS0` bank of voice lines, or the sounds of one kind that a
/// sound map lists for one area, such as `1_1_0Tbilisi`'s sound effects.
#[derive(Clone, Debug)]
pub struct Bank {
    pub name: String,
    /// The extra map whose folder holds the bank, such as `1_6_1_1kolacell`,
    /// or `None` for the language's own banks. Maps reuse bank names.
    pub map: Option<String>,
    /// The sound map's area the bank's sounds are from, such as
    /// `1_1_0Tbilisi`, or `None` for an `.LS0` bank.
    pub area: Option<String>,
    /// The `.LS0` bank or sound map, relative to `Sounds`.
    pub path: String,
    pub code: String,
    pub mission_name: Option<String>,
    /// The size of the file at `path`.
    pub size: u64,
    pub issue: Option<String>,
    pub segments: Vec<Segment>,
}

#[derive(Clone, Debug)]
pub struct Segment {
    pub id: String,
    pub source: SegmentSource,
    /// The sound's name in its map, such as `ROGEAR_2`, or `None` for a
    /// stream in an `.LS0` bank.
    pub name: Option<String>,
    pub sample_count: u32,
    pub sample_rate: u32,
    pub channels: u32,
    pub codec: String,
    pub segment: usize,
}

/// Where a segment's audio is in its bank's file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SegmentSource {
    /// A stream found by scanning an `.LS0` bank.
    Stream {
        offset: u64,
        /// The length of the stream itself. Banks pad streams to 4-byte
        /// boundaries, and the padding isn't included.
        byte_length: u64,
    },
    /// A sound in a sound map, which vgmstream finds by its number.
    MapSound {
        /// The sound's number in the map, from 1.
        subsong: u32,
        /// The map's key for the sound, such as `0x00130086`.
        key: Option<u32>,
    },
}

impl Segment {
    /// The offset and length of a stream in an `.LS0` bank.
    pub fn stream_range(&self) -> Option<(u64, u64)> {
        match self.source {
            SegmentSource::Stream {
                offset,
                byte_length,
            } => Some((offset, byte_length)),
            SegmentSource::MapSound { .. } => None,
        }
    }
}

pub fn discover_languages(root: &Path) -> Result<Vec<Language>, Error> {
    let sounds = find_sounds_directory(root)?;
    let entries = read_directory(&sounds)?;
    let mut languages = Vec::new();

    for entry in entries {
        let file_type = entry
            .file_type()
            .map_err(|error| format!("Could not inspect {}: {error}", entry.path().display()))?;
        if !file_type.is_dir() {
            continue;
        }

        // Extra maps keep their own sounds in folders beside the languages,
        // such as `1_6_1_1kolacell`, so only known language names count.
        let directory = entry.file_name().to_string_lossy().into_owned();
        let Some(known) = known_language(&directory) else {
            continue;
        };
        let mut banks = Vec::new();
        collect_banks(&entry.path(), Path::new(""), &mut banks)?;
        if !banks.is_empty() {
            languages.push(Language {
                label: known.label.to_owned(),
                directory,
            });
        }
    }

    languages.sort_by(|left, right| {
        left.label
            .to_ascii_lowercase()
            .cmp(&right.label.to_ascii_lowercase())
    });
    if languages.is_empty() {
        return Err(format!(
            "No numeric .LS0 sound banks were found under {}.",
            sounds.display()
        )
        .into());
    }
    Ok(languages)
}

pub fn discover_banks(root: &Path, language: &str) -> Result<Vec<Bank>, Error> {
    static NEVER_CANCELLED: AtomicBool = AtomicBool::new(false);
    discover_banks_with_progress(root, language, &NEVER_CANCELLED, |_, _, _| {})
}

pub fn discover_banks_with_progress(
    root: &Path,
    language: &str,
    cancelled: &AtomicBool,
    mut progress: impl FnMut(usize, usize, &str),
) -> Result<Vec<Bank>, Error> {
    let sounds = find_sounds_directory(root)?;
    let mut folders = vec![LanguageFolder {
        map: None,
        relative_path: language.to_owned(),
    }];
    if known_language(language).is_none() || !safe_child_path(&sounds, language)?.is_dir() {
        return Err(format!("Language folder was not found: {language}").into());
    }
    folders.extend(map_language_folders(&sounds, language)?);

    /// A file to read banks from.
    enum Source {
        Bank(PathBuf, PathBuf),
        SoundMap(String),
    }
    let mut sources = Vec::new();
    for (folder_index, folder) in folders.iter().enumerate() {
        let mut folder_files = Vec::new();
        let absolute_path = safe_child_path(&sounds, &folder.relative_path)?;
        collect_banks(
            &absolute_path,
            Path::new(&folder.relative_path),
            &mut folder_files,
        )?;
        sources.extend(
            folder_files
                .into_iter()
                .map(|(absolute, relative)| (folder_index, Source::Bank(absolute, relative))),
        );
        sources.extend(
            sound_maps::map_files(&sounds, folder.map.as_deref(), &folder.relative_path)
                .into_iter()
                .map(|path| (folder_index, Source::SoundMap(path))),
        );
    }

    let mut banks = Vec::new();
    let total = sources.len();
    progress(0, total, "");
    for (index, (folder_index, source)) in sources.into_iter().enumerate() {
        if cancelled.load(Ordering::Relaxed) {
            return Err(Error::Cancelled);
        }
        let map = folders[folder_index].map.as_deref();
        let name = match source {
            Source::Bank(absolute_path, relative_path) => {
                let bank = read_bank(&absolute_path, &relative_path, map, cancelled)?;
                let name = bank.name.clone();
                banks.push((folder_index, 0, bank));
                name
            }
            Source::SoundMap(path) => {
                for (kind, bank) in sound_maps::read_map_banks(&sounds, &path, map, cancelled)? {
                    let rank = kind.map_or(u8::MAX, |kind| kind as u8 + 1);
                    banks.push((folder_index, rank, bank));
                }
                path
            }
        };
        progress(index + 1, total, &name);
    }
    // Banks are listed by folder, then by mission code, so a level's voice
    // lines come before its map sounds. Banks without a code, such as the
    // maps' `fix` and `menu` areas, go last.
    banks.sort_by_cached_key(|(folder_index, rank, bank)| {
        let code: Option<Vec<u32>> = bank.code.split('_').map(|part| part.parse().ok()).collect();
        (
            *folder_index,
            code.unwrap_or_else(|| vec![u32::MAX]),
            *rank,
            bank.name.to_ascii_lowercase(),
        )
    });

    let names = mission_names(&sounds, language);
    Ok(banks
        .into_iter()
        .map(|(_, _, mut bank)| {
            // Extra maps reuse bank names from other missions, so their
            // mission is the map's, not the bank file's. The map folder name
            // (`1_7_1_1VselkaInfiltration`) starts with its mission code.
            bank.mission_name = match &bank.map {
                Some(map) => {
                    leading_mission_code(map).and_then(|map_code| names.get(&map_code).cloned())
                }
                None => mission_name(&names, &bank.code),
            };
            bank
        })
        .collect())
}

/// Scans the `.LS0` bank at `absolute_path` for streams.
fn read_bank(
    absolute_path: &Path,
    relative_path: &Path,
    map: Option<&str>,
    cancelled: &AtomicBool,
) -> Result<Bank, Error> {
    let bytes = fs::read(absolute_path)
        .map_err(|error| format!("Could not read {}: {error}", absolute_path.display()))?;
    let name = absolute_path
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| format!("Bank path is not valid UTF-8: {}", absolute_path.display()))?
        .to_owned();
    let path = relative_path.to_string_lossy().replace('\\', "/");
    let segments = scan_bank(&bytes, &path, cancelled)?;
    let issue = if segments.is_empty() {
        Some(diagnose_unrecognized_bank(&bytes))
    } else {
        None
    };
    Ok(Bank {
        code: mission_code(&name).unwrap_or_default(),
        name,
        map: map.map(str::to_owned),
        area: None,
        path,
        mission_name: None,
        size: bytes.len() as u64,
        issue,
        segments,
    })
}

/// Reads and decodes `segment`, from the bank or sound map at `bank_path`
/// under the game's `Sounds` folder.
pub fn decode_segment(
    root: &Path,
    bank_path: &str,
    segment: &Segment,
    cancelled: &AtomicBool,
) -> Result<DecodedAudio, Error> {
    match segment.source {
        SegmentSource::Stream {
            offset,
            byte_length,
        } => {
            let bytes = read_segment_with_cancel(root, bank_path, offset, byte_length, cancelled)?;
            decoder::decode(&bytes, segment.sample_count, cancelled)
        }
        SegmentSource::MapSound { subsong, .. } => {
            let sounds = find_sounds_directory(root)?;
            decoder::decode_map_sound(&sounds, bank_path, subsong, cancelled)
        }
    }
}

pub fn read_segment(
    root: &Path,
    relative_path: &str,
    offset: u64,
    byte_length: u64,
) -> Result<Vec<u8>, Error> {
    read_segment_inner(root, relative_path, offset, byte_length, None)
}

pub fn read_segment_with_cancel(
    root: &Path,
    relative_path: &str,
    offset: u64,
    byte_length: u64,
    cancelled: &AtomicBool,
) -> Result<Vec<u8>, Error> {
    read_segment_inner(root, relative_path, offset, byte_length, Some(cancelled))
}

fn read_segment_inner(
    root: &Path,
    relative_path: &str,
    offset: u64,
    byte_length: u64,
    cancelled: Option<&AtomicBool>,
) -> Result<Vec<u8>, Error> {
    let sounds = find_sounds_directory(root)?;
    let file_path = safe_child_path(&sounds, relative_path)?;
    let mut file = fs::File::open(&file_path)
        .map_err(|error| format!("Could not open {}: {error}", file_path.display()))?;
    let metadata = file
        .metadata()
        .map_err(|error| format!("Could not inspect {}: {error}", file_path.display()))?;
    if !metadata.is_file() {
        return Err(format!(
            "The requested segment source is not a file: {}",
            file_path.display()
        )
        .into());
    }
    let end = offset
        .checked_add(byte_length)
        .ok_or_else(|| "Segment range overflows the file address space.".to_owned())?;
    if end > metadata.len() {
        return Err(format!(
            "Segment range {offset}..{end} exceeds {} ({} bytes).",
            file_path.display(),
            metadata.len()
        )
        .into());
    }

    let length = usize::try_from(byte_length)
        .map_err(|_| "Segment length is too large to read.".to_owned())?;
    if cancelled.is_some_and(|flag| flag.load(Ordering::Relaxed)) {
        return Err(Error::Cancelled);
    }
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(length)
        .map_err(|error| format!("Could not allocate {length} bytes for the segment: {error}"))?;
    bytes.resize(length, 0);

    file.seek(SeekFrom::Start(offset))
        .map_err(|error| format!("Could not seek in {}: {error}", file_path.display()))?;

    const READ_CHUNK_SIZE: usize = 64 * 1024;
    for chunk in bytes.chunks_mut(READ_CHUNK_SIZE) {
        if cancelled.is_some_and(|flag| flag.load(Ordering::Relaxed)) {
            return Err(Error::Cancelled);
        }
        file.read_exact(chunk).map_err(|error| {
            format!(
                "Could not read segment from {}: {error}",
                file_path.display()
            )
        })?;
    }
    if cancelled.is_some_and(|flag| flag.load(Ordering::Relaxed)) {
        return Err(Error::Cancelled);
    }

    Ok(bytes)
}

fn find_sounds_directory(root: &Path) -> Result<PathBuf, String> {
    let root = root
        .canonicalize()
        .map_err(|error| format!("Could not open selected folder {}: {error}", root.display()))?;
    if root
        .file_name()
        .is_some_and(|name| name.to_string_lossy().eq_ignore_ascii_case("sounds"))
    {
        return Ok(root);
    }

    for name in ["Sounds", "sounds"] {
        let candidate = root.join(name);
        if candidate.is_dir() {
            return candidate.canonicalize().map_err(|error| {
                format!(
                    "Could not open sound folder {}: {error}",
                    candidate.display()
                )
            });
        }
    }
    Err(format!(
        "No Sounds directory was found in {}. Select the game installation or its Sounds folder.",
        root.display()
    ))
}

fn safe_child_path(root: &Path, relative: &str) -> Result<PathBuf, String> {
    let relative_path = Path::new(relative);
    if relative_path.as_os_str().is_empty()
        || relative_path
            .components()
            .any(|component| !matches!(component, Component::Normal(_) | Component::CurDir))
    {
        return Err("The requested sound-bank path is invalid.".to_owned());
    }
    let candidate = root.join(relative_path);
    let canonical = candidate
        .canonicalize()
        .map_err(|error| format!("Could not open {}: {error}", candidate.display()))?;
    if !canonical.starts_with(root) {
        return Err("The requested path is outside the selected Sounds directory.".to_owned());
    }
    Ok(canonical)
}

fn read_directory(path: &Path) -> Result<Vec<fs::DirEntry>, String> {
    fs::read_dir(path)
        .map_err(|error| format!("Could not read directory {}: {error}", path.display()))?
        .map(|entry| {
            entry.map_err(|error| format!("Could not read an entry in {}: {error}", path.display()))
        })
        .collect()
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

fn collect_banks(
    directory: &Path,
    relative_directory: &Path,
    output: &mut Vec<(PathBuf, PathBuf)>,
) -> Result<(), String> {
    for entry in read_directory(directory)? {
        let file_type = entry
            .file_type()
            .map_err(|error| format!("Could not inspect {}: {error}", entry.path().display()))?;
        let name = entry.file_name();
        let relative_path = relative_directory.join(&name);
        if file_type.is_dir() {
            collect_banks(&entry.path(), &relative_path, output)?;
        } else if file_type.is_file() && is_supported_bank_name(&name.to_string_lossy()) {
            output.push((entry.path(), relative_path));
        }
    }
    Ok(())
}

/// A folder of one language's banks, relative to `Sounds`.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct LanguageFolder {
    /// The extra map the folder belongs to, or `None` for the language's own
    /// folder.
    pub(crate) map: Option<String>,
    pub(crate) relative_path: String,
}

/// Finds `language`'s folders in the extra maps beside the languages under
/// `Sounds`, such as `1_6_1_1kolacell/English`, sorted by map name. Each map
/// has its own banks and `MAPS.LM0` for each language it ships.
pub(crate) fn map_language_folders(
    sounds: &Path,
    language: &str,
) -> Result<Vec<LanguageFolder>, String> {
    let Some(language) = known_language(language) else {
        return Ok(Vec::new());
    };
    let mut folders = Vec::new();
    for entry in read_directory(sounds)? {
        let map = entry.file_name().to_string_lossy().into_owned();
        if !entry.path().is_dir() || known_language(&map).is_some() {
            continue;
        }
        for child in read_directory(&entry.path())? {
            let child_name = child.file_name().to_string_lossy().into_owned();
            if child.path().is_dir() && known_language(&child_name).as_ref() == Some(&language) {
                folders.push(LanguageFolder {
                    relative_path: format!("{map}/{child_name}"),
                    map: Some(map.clone()),
                });
            }
        }
    }
    folders.sort_by(|left, right| {
        left.relative_path
            .to_ascii_lowercase()
            .cmp(&right.relative_path.to_ascii_lowercase())
    });
    Ok(folders)
}

#[derive(Debug, PartialEq, Eq)]
struct KnownLanguage {
    label: &'static str,
    /// The extension of the game's localization files, such as `Echelon.int`.
    localization_extension: &'static str,
}

/// Looks up a language by its folder under `Sounds`, ignoring case.
fn known_language(directory: &str) -> Option<KnownLanguage> {
    let (label, localization_extension) = match directory.to_ascii_uppercase().as_str() {
        "ENGLISH" => ("English", "int"),
        "FRENCH" => ("French", "fra"),
        "GERMAN" => ("German", "deu"),
        // The game names this folder in French.
        "ITALIEN" | "ITALIAN" => ("Italian", "ita"),
        "SPANISH" => ("Spanish", "esp"),
        _ => return None,
    };
    Some(KnownLanguage {
        label,
        localization_extension,
    })
}

fn mission_code(file_name: &str) -> Option<String> {
    let stem = file_name
        .rsplit_once('.')
        .filter(|(_, extension)| extension.eq_ignore_ascii_case("LS0"))
        .map_or(file_name, |(stem, _)| stem);
    let valid = stem
        .split('_')
        .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()));
    if valid && stem.contains('_') {
        Some(stem.to_owned())
    } else {
        None
    }
}

/// The display name of the mission with `code`. Shared banks (`1_1`), whose
/// lines several levels play, aren't named in `HUD`, so they take their
/// mission's first level's name (`1_1_0`).
fn mission_name(names: &BTreeMap<String, String>, code: &str) -> Option<String> {
    if let Some(name) = names.get(code) {
        return Some(name.clone());
    }
    if code.matches('_').count() != 1 {
        return None;
    }
    let prefix = format!("{code}_");
    names
        .range(prefix.clone()..)
        .next()
        .filter(|(level, _)| level.starts_with(&prefix))
        .map(|(_, name)| name.clone())
}

/// Reads the mission display names from the game's `HUD` localization file in
/// `language`, or the English one when that's missing, keyed by mission code
/// such as `1_1_0`. Empty when neither file can be read.
fn mission_names(sounds: &Path, language: &str) -> BTreeMap<String, String> {
    let Some(game_root) = sounds.parent() else {
        return BTreeMap::new();
    };
    let localization_root = game_root.join("system").join("LOCALIZATION");
    let extension = known_language(language).map_or("int", |known| known.localization_extension);
    let Some(bytes) = [extension, "int"].into_iter().find_map(|extension| {
        subtitles::localization::localization_path(&localization_root, "HUD", extension)
            .ok()
            .and_then(|path| fs::read(path).ok())
    }) else {
        return BTreeMap::new();
    };
    let (text, _, _) = WINDOWS_1252.decode(&bytes);
    let mut sections = HashMap::new();
    subtitles::localization::parse_sections(&text, &mut sections);
    sections
        .remove("hud")
        .into_iter()
        .flatten()
        .filter_map(|(key, name)| Some((leading_mission_code(&key)?, name)))
        .collect()
}

/// The mission code a `HUD` key or extra map folder starts with, such as
/// `1_1_0` from `1_1_0Tbilisi` or `0_0_2` from `0_0_2_Training`.
fn leading_mission_code(name: &str) -> Option<String> {
    let end = name
        .find(|c: char| !c.is_ascii_digit() && c != '_')
        .unwrap_or(name.len());
    mission_code(name[..end].trim_end_matches('_'))
}

fn is_supported_bank_name(file_name: &str) -> bool {
    mission_code(file_name).is_some()
}

fn read_u32_le(bytes: &[u8], offset: usize) -> Option<u32> {
    let end = offset.checked_add(4)?;
    let value: [u8; 4] = bytes.get(offset..end)?.try_into().ok()?;
    Some(u32::from_le_bytes(value))
}

fn has_valid_frame(bytes: &[u8], offset: usize) -> bool {
    offset
        .checked_add(FRAME_HEADER_SIZE)
        .is_some_and(|end| end <= bytes.len())
        && read_u32_le(bytes, offset) == Some(2)
}

fn scan_bank(bytes: &[u8], bank_path: &str, cancelled: &AtomicBool) -> Result<Vec<Segment>, Error> {
    let mut segments: Vec<Segment> = Vec::new();
    let mut offset = 0usize;

    while offset
        .checked_add(BANK_HEADER_SIZE)
        .is_some_and(|end| end <= bytes.len())
    {
        if offset & 0x0fff == 0 && cancelled.load(Ordering::Relaxed) {
            return Err(Error::Cancelled);
        }
        if read_u32_le(bytes, offset) != Some(8) {
            offset += 1;
            continue;
        }

        let Some(sample_count) = read_u32_le(bytes, offset + 4) else {
            offset += 1;
            continue;
        };
        let Some(subframe_count) = read_u32_le(bytes, offset + 8) else {
            offset += 1;
            continue;
        };
        let Some(last_code_count) = read_u32_le(bytes, offset + 12) else {
            offset += 1;
            continue;
        };
        let Some(code_count) = read_u32_le(bytes, offset + 16) else {
            offset += 1;
            continue;
        };
        let Some(subframes_per_frame) = read_u32_le(bytes, offset + 20) else {
            offset += 1;
            continue;
        };
        let sample_rate = read_u32_le(bytes, offset + 24).unwrap_or(0);
        let bits_per_sample = read_u32_le(bytes, offset + 36).unwrap_or(0);
        let channels = read_u32_le(bytes, offset + 44).unwrap_or(0);

        if sample_count == 0
            || subframe_count == 0
            || subframes_per_frame != 2
            || code_count == 0
            || code_count > CODES_PER_FRAME
            || last_code_count > CODES_PER_FRAME
            || !matches!(bits_per_sample, 4 | 6)
            || channels != 1
            || !offset
                .checked_add(BANK_HEADER_SIZE + FRAME_HEADER_SIZE)
                .is_some_and(|end| end <= bytes.len())
            || !has_valid_frame(bytes, offset + BANK_HEADER_SIZE)
        {
            offset += 1;
            continue;
        }

        let mut frame_offset = offset + BANK_HEADER_SIZE;
        let mut subframe_number = 0u32;
        let mut decoded_code_count = 0u32;
        let mut valid_frames = true;

        while subframe_number < subframe_count {
            if cancelled.load(Ordering::Relaxed) {
                return Err(Error::Cancelled);
            }
            if !has_valid_frame(bytes, frame_offset) {
                valid_frames = false;
                break;
            }

            let (first_count, second_count) = if subframe_number + 1 == subframe_count {
                (last_code_count, 0)
            } else if subframe_number + 2 == subframe_count {
                (code_count, last_code_count)
            } else {
                (code_count, code_count)
            };
            let first_payload_length = (first_count as usize * bits_per_sample as usize) / 8 + 1;
            let second_payload_length = if second_count == 0 {
                0
            } else {
                (second_count as usize * bits_per_sample as usize) / 8 + 1
            };
            let Some(frame_length) = FRAME_HEADER_SIZE
                .checked_add(first_payload_length)
                .and_then(|value| value.checked_add(second_payload_length))
            else {
                valid_frames = false;
                break;
            };
            let Some(next_frame_offset) = frame_offset.checked_add(frame_length) else {
                valid_frames = false;
                break;
            };
            if next_frame_offset > bytes.len() {
                valid_frames = false;
                break;
            }

            let Some(next_decoded_count) = decoded_code_count
                .checked_add(first_count)
                .and_then(|value| value.checked_add(second_count))
            else {
                valid_frames = false;
                break;
            };
            decoded_code_count = next_decoded_count;
            frame_offset = next_frame_offset;
            subframe_number += subframes_per_frame;
        }

        let byte_length = frame_offset.saturating_sub(offset);
        if !valid_frames
            || decoded_code_count < sample_count
            || byte_length <= BANK_HEADER_SIZE + FRAME_HEADER_SIZE
        {
            offset += 1;
            continue;
        }

        segments.push(Segment {
            id: format!("{bank_path}:{offset}"),
            source: SegmentSource::Stream {
                offset: offset as u64,
                byte_length: byte_length as u64,
            },
            name: None,
            sample_count,
            sample_rate: if sample_rate == 0 { 36000 } else { sample_rate },
            channels,
            codec: if bits_per_sample == 6 {
                "Ubisoft ADPCM 6-bit"
            } else {
                "Ubisoft ADPCM 4-bit"
            }
            .to_owned(),
            segment: segments.len(),
        });
        offset = frame_offset;
    }

    if cancelled.load(Ordering::Relaxed) {
        return Err(Error::Cancelled);
    }
    Ok(segments)
}

fn diagnose_unrecognized_bank(bytes: &[u8]) -> String {
    if bytes.is_empty() {
        return "This bank is empty.".to_owned();
    }
    if read_u32_le(bytes, 0) == Some(8) {
        if bytes.len() < BANK_HEADER_SIZE {
            return "The bank is truncated before its complete stream header.".to_owned();
        }
        let bits_per_sample = read_u32_le(bytes, 36).unwrap_or(0);
        let channels = read_u32_le(bytes, 44).unwrap_or(0);
        if !matches!(bits_per_sample, 4 | 6) || channels != 1 {
            return format!(
                "Unsupported stream shape: {bits_per_sample}-bit, {channels} channels. This tool supports mono 4-bit and 6-bit streams."
            );
        }
        return format!(
            "A mono {bits_per_sample}-bit stream header was found, but no complete valid frames could be parsed; the bank may be malformed or truncated."
        );
    }
    "No supported mono 4-bit or 6-bit streams were recognized; this bank may use an unsupported layout or contain malformed data.".to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// A minimal valid segment: one mono 4-bit subframe of four samples at
    /// 36 kHz, followed by its frame header and code bytes.
    pub(crate) fn make_segment() -> Vec<u8> {
        make_segment_with_bits(4)
    }

    /// Like [`make_segment`], with `bits` per code.
    pub(crate) fn make_segment_with_bits(bits: u32) -> Vec<u8> {
        let codes_length = (4 * bits as usize) / 8 + 1;
        let mut bytes = vec![0; BANK_HEADER_SIZE + FRAME_HEADER_SIZE + codes_length];
        for (offset, value) in [
            (0, 8_u32),
            (4, 4),
            (8, 1),
            (12, 4),
            (16, 4),
            (20, 2),
            (24, 36000),
            (36, bits),
            (44, 1),
            (48, 2),
        ] {
            bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
        }
        bytes
    }

    #[test]
    fn scans_segments_with_offsets_sizes_and_metadata() {
        let first = make_segment();
        let second = make_segment();
        let mut bytes = first.clone();
        bytes.extend_from_slice(&[0; 5]);
        bytes.extend_from_slice(&second);

        let cancelled = AtomicBool::new(false);
        let segments = scan_bank(&bytes, "ENGLISH/1_2.LS0", &cancelled).unwrap();
        assert_eq!(segments.len(), 2);
        assert_eq!(segments[0].stream_range(), Some((0, 103)));
        assert_eq!(segments[0].id, "ENGLISH/1_2.LS0:0");
        assert_eq!(segments[0].sample_count, 4);
        assert_eq!(segments[0].sample_rate, 36000);
        assert_eq!(segments[0].channels, 1);
        assert_eq!(segments[1].stream_range(), Some((108, 103)));
    }

    #[test]
    fn rejects_non_numeric_banks_and_malformed_streams() {
        assert_eq!(mission_code("1_2_3.LS0").as_deref(), Some("1_2_3"));
        assert_eq!(mission_code("Music_Common.LS0"), None);
        assert_eq!(mission_code("1_2_3.uax"), None);
        let cancelled = AtomicBool::new(false);
        assert!(
            scan_bank(&[0; 200], "ENGLISH/1_2.LS0", &cancelled)
                .unwrap()
                .is_empty()
        );
        assert!(diagnose_unrecognized_bank(&[0; 200]).contains("unsupported layout"));

        let mut truncated = make_segment();
        truncated.truncate(80);
        assert!(
            scan_bank(&truncated, "ENGLISH/1_2.LS0", &cancelled)
                .unwrap()
                .is_empty()
        );
        assert!(diagnose_unrecognized_bank(&truncated).contains("malformed or truncated"));
    }

    #[test]
    fn rejects_cancelled_bank_scans() {
        let game = tempfile::tempdir().unwrap();
        let english = game.path().join("Sounds/ENGLISH");
        fs::create_dir_all(&english).unwrap();
        fs::write(english.join("1_1_0.LS0"), make_segment()).unwrap();
        let cancelled = AtomicBool::new(true);

        let error = discover_banks_with_progress(game.path(), "ENGLISH", &cancelled, |_, _, _| {})
            .unwrap_err();
        assert_eq!(error, Error::Cancelled);
    }

    #[test]
    fn discovers_languages_and_banks_from_selected_game_directory() {
        let game = tempfile::tempdir().unwrap();
        let english = game.path().join("Sounds/ENGLISH");
        let german = game.path().join("Sounds/GERMAN");
        fs::create_dir_all(&english).unwrap();
        fs::create_dir_all(&german).unwrap();
        fs::write(english.join("1_1_0.LS0"), make_segment()).unwrap();
        fs::write(english.join("2_2.LS0"), [0; 200]).unwrap();
        fs::write(english.join("Music_Common.LS0"), make_segment()).unwrap();
        fs::write(german.join("1_1_0.LS0"), make_segment()).unwrap();
        fs::write(game.path().join("Sounds/1_1_0.LS0"), make_segment()).unwrap();

        let languages = discover_languages(game.path()).unwrap();
        assert_eq!(
            languages
                .iter()
                .map(|language| language.directory.as_str())
                .collect::<Vec<_>>(),
            ["ENGLISH", "GERMAN"]
        );
        assert!(discover_banks(game.path(), ".").is_err());
        let banks = discover_banks(game.path(), "ENGLISH").unwrap();
        assert_eq!(banks.len(), 2);
        let supported = banks.iter().find(|bank| bank.name == "1_1_0.LS0").unwrap();
        assert_eq!(supported.path, "ENGLISH/1_1_0.LS0");
        assert_eq!(supported.issue, None);
        assert_eq!(supported.segments.len(), 1);
        assert_eq!(supported.segments[0].stream_range(), Some((0, 103)));
        let unsupported = banks.iter().find(|bank| bank.name == "2_2.LS0").unwrap();
        assert!(unsupported.segments.is_empty());
        assert!(
            unsupported
                .issue
                .as_deref()
                .unwrap()
                .contains("unsupported layout")
        );
    }

    #[test]
    fn reads_mission_names_from_the_games_hud_localization() {
        let game = tempfile::tempdir().unwrap();
        let english = game.path().join("Sounds/ENGLISH");
        fs::create_dir_all(&english).unwrap();
        for bank in ["1_1_0.LS0", "1_1.LS0", "4_2.LS0", "3_1_1.LS0"] {
            fs::write(english.join(bank), make_segment()).unwrap();
        }
        // An extra map reuses a bank name from another mission.
        let vselka = game.path().join("Sounds/1_7_1_2Vselka/English");
        fs::create_dir_all(&vselka).unwrap();
        fs::write(vselka.join("3_1_1.LS0"), make_segment()).unwrap();
        let localization = game.path().join("system/LOCALIZATION");
        fs::create_dir_all(&localization).unwrap();
        fs::write(
            localization.join("HUD.int"),
            "[HUD]\n1_1_0Tbilisi=Police Station\n1_1_1Tbilisi=Police Station\n4_2_1_Abattoir=Abattoir\n1_7_1_2Vselka=Vselka Submarine\n",
        )
        .unwrap();

        let banks = discover_banks(game.path(), "ENGLISH").unwrap();
        let name = |file_name: &str| {
            banks
                .iter()
                .find(|bank| bank.name == file_name)
                .unwrap()
                .mission_name
                .clone()
        };
        // A level bank uses its own entry.
        assert_eq!(name("1_1_0.LS0").as_deref(), Some("Police Station"));
        // A shared bank uses its mission's first level's name.
        assert_eq!(name("1_1.LS0").as_deref(), Some("Police Station"));
        assert_eq!(name("4_2.LS0").as_deref(), Some("Abattoir"));
        // A level bank with no HUD entry has no name.
        assert_eq!(name("3_1_1.LS0"), None);
        // An extra map's bank takes its mission from the map, not the reused
        // bank name.
        let vselka_bank = banks
            .iter()
            .find(|bank| bank.map.as_deref() == Some("1_7_1_2Vselka"))
            .unwrap();
        assert_eq!(
            vselka_bank.mission_name.as_deref(),
            Some("Vselka Submarine")
        );
    }

    #[test]
    fn reads_mission_names_in_the_banks_language() {
        let game = tempfile::tempdir().unwrap();
        for language in ["FRENCH", "GERMAN"] {
            let folder = game.path().join("Sounds").join(language);
            fs::create_dir_all(&folder).unwrap();
            fs::write(folder.join("1_1_0.LS0"), make_segment()).unwrap();
        }
        let localization = game.path().join("system/LOCALIZATION");
        fs::create_dir_all(&localization).unwrap();
        fs::write(
            localization.join("HUD.int"),
            "[HUD]\n1_1_0Tbilisi=Police Station\n",
        )
        .unwrap();
        fs::write(
            localization.join("HUD.fra"),
            b"[HUD]\r\n1_1_0Tbilisi=Poste de Police\r\n0_0_2_Training=Entra\xeenement\r\n",
        )
        .unwrap();

        let name = |language: &str| {
            discover_banks(game.path(), language).unwrap()[0]
                .mission_name
                .clone()
        };
        assert_eq!(name("FRENCH").as_deref(), Some("Poste de Police"));
        // A language without its own `HUD` file falls back to English.
        assert_eq!(name("GERMAN").as_deref(), Some("Police Station"));
    }

    #[test]
    fn finds_extra_map_banks_under_their_language_instead_of_as_languages() {
        let game = tempfile::tempdir().unwrap();
        let italian = game.path().join("Sounds/ITALIEN");
        let extra_map = game.path().join("Sounds/1_6_1_1kolacell");
        fs::create_dir_all(&italian).unwrap();
        fs::create_dir_all(extra_map.join("English")).unwrap();
        fs::create_dir_all(extra_map.join("Italien")).unwrap();
        fs::write(italian.join("1_1_0.LS0"), make_segment()).unwrap();
        fs::write(extra_map.join("English/3_4_2.LS0"), make_segment()).unwrap();
        // Extra maps' banks use 6-bit codes.
        fs::write(
            extra_map.join("Italien/3_4_2.LS0"),
            make_segment_with_bits(6),
        )
        .unwrap();

        let languages = discover_languages(game.path()).unwrap();
        assert_eq!(
            languages
                .iter()
                .map(|language| (language.directory.as_str(), language.label.as_str()))
                .collect::<Vec<_>>(),
            [("ITALIEN", "Italian")]
        );

        let banks = discover_banks(game.path(), "ITALIEN").unwrap();
        assert_eq!(
            banks
                .iter()
                .map(|bank| (bank.path.as_str(), bank.map.as_deref()))
                .collect::<Vec<_>>(),
            [
                ("ITALIEN/1_1_0.LS0", None),
                ("1_6_1_1kolacell/Italien/3_4_2.LS0", Some("1_6_1_1kolacell")),
            ]
        );
        assert_eq!(
            banks[1].segments[0].id,
            "1_6_1_1kolacell/Italien/3_4_2.LS0:0"
        );
        assert_eq!(banks[1].segments[0].codec, "Ubisoft ADPCM 6-bit");
        let cancelled = AtomicBool::new(false);
        let decoded = decode_segment(
            game.path(),
            &banks[1].path,
            &banks[1].segments[0],
            &cancelled,
        )
        .unwrap();
        assert_eq!(decoded.samples.len(), 4);
    }

    #[test]
    fn segment_reads_are_bounded_to_the_sounds_directory() {
        let game = tempfile::tempdir().unwrap();
        let sounds = game.path().join("Sounds/ENGLISH");
        fs::create_dir_all(&sounds).unwrap();
        fs::write(sounds.join("1_1_0.LS0"), make_segment()).unwrap();

        let data = read_segment(game.path(), "ENGLISH/1_1_0.LS0", 48, 55).unwrap();
        assert_eq!(data, make_segment()[48..].to_vec());
        assert!(read_segment(game.path(), "ENGLISH", 0, 1).is_err());
        assert!(read_segment(game.path(), "../outside", 0, 1).is_err());
        assert!(read_segment(game.path(), "ENGLISH/1_1_0.LS0", 100, 20).is_err());
    }

    #[test]
    fn segment_read_honours_cancellation() {
        let game = tempfile::tempdir().unwrap();
        let sounds = game.path().join("Sounds/ENGLISH");
        fs::create_dir_all(&sounds).unwrap();
        fs::write(sounds.join("1_1_0.LS0"), make_segment()).unwrap();
        let cancelled = AtomicBool::new(true);

        let error = read_segment_with_cancel(game.path(), "ENGLISH/1_1_0.LS0", 0, 103, &cancelled)
            .unwrap_err();
        assert_eq!(error, Error::Cancelled);
    }
}
