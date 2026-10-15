pub mod decoder;
mod error;
mod sound_keys;
pub mod subtitles;

pub use error::Error;

use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

const BANK_HEADER_SIZE: usize = 0x30;
const FRAME_HEADER_SIZE: usize = 0x34;
const CODES_PER_FRAME: u32 = 1536;

#[derive(Clone, Debug)]
pub struct Language {
    pub directory: String,
    pub label: String,
}

#[derive(Clone, Debug)]
pub struct Bank {
    pub name: String,
    /// The extra map whose folder holds the bank, such as `1_6_1_1kolacell`,
    /// or `None` for the language's own banks. Maps reuse bank names.
    pub map: Option<String>,
    pub path: String,
    pub code: String,
    pub size: u64,
    pub issue: Option<String>,
    pub segments: Vec<Segment>,
}

#[derive(Clone, Debug)]
pub struct Segment {
    pub id: String,
    pub language: String,
    pub bank: String,
    pub code: String,
    pub offset: u64,
    /// The length of the stream itself. Banks pad streams to 4-byte
    /// boundaries, and the padding isn't included.
    pub byte_length: u64,
    pub sample_count: u32,
    pub sample_rate: u32,
    pub channels: u32,
    pub codec: &'static str,
    pub segment: usize,
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

    let mut root_banks = Vec::new();
    for entry in read_directory(&sounds)? {
        let file_type = entry
            .file_type()
            .map_err(|error| format!("Could not inspect {}: {error}", entry.path().display()))?;
        if file_type.is_file() && is_supported_bank_name(&entry.file_name().to_string_lossy()) {
            root_banks.push(entry.path());
        }
    }
    if !root_banks.is_empty() {
        languages.push(Language {
            directory: ".".to_owned(),
            label: "Sounds root".to_owned(),
        });
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
    if language != "." {
        if !safe_child_path(&sounds, language)?.is_dir() {
            return Err(format!("Language folder was not found: {language}").into());
        }
        folders.extend(map_language_folders(&sounds, language)?);
    }

    let mut files = Vec::new();
    for folder in &folders {
        let mut folder_files = Vec::new();
        let absolute_path = safe_child_path(&sounds, &folder.relative_path)?;
        collect_banks(
            &absolute_path,
            Path::new(&folder.relative_path),
            &mut folder_files,
        )?;
        folder_files.sort_by(|left, right| {
            left.1
                .to_string_lossy()
                .to_ascii_lowercase()
                .cmp(&right.1.to_string_lossy().to_ascii_lowercase())
        });
        files.extend(
            folder_files
                .into_iter()
                .map(|(absolute, relative)| (absolute, relative, folder.map.clone())),
        );
    }

    let mut banks = Vec::with_capacity(files.len());
    let total = files.len();
    progress(0, total, "");
    for (index, (absolute_path, relative_path, map)) in files.into_iter().enumerate() {
        if cancelled.load(Ordering::Relaxed) {
            return Err(Error::Cancelled);
        }
        let bytes = fs::read(&absolute_path)
            .map_err(|error| format!("Could not read {}: {error}", absolute_path.display()))?;
        let name = absolute_path
            .file_name()
            .and_then(|value| value.to_str())
            .ok_or_else(|| format!("Bank path is not valid UTF-8: {}", absolute_path.display()))?
            .to_owned();
        let code = mission_code(&name).unwrap_or_default();
        let path = relative_path.to_string_lossy().replace('\\', "/");
        let segments = scan_bank(&bytes, &name, &path, language, &code, cancelled)?;
        let issue = if segments.is_empty() {
            Some(diagnose_unrecognized_bank(&bytes))
        } else {
            None
        };
        banks.push(Bank {
            name,
            map,
            path,
            code,
            size: bytes.len() as u64,
            issue,
            segments,
        });
        progress(index + 1, total, &banks[index].name);
    }
    Ok(banks)
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

fn scan_bank(
    bytes: &[u8],
    bank_name: &str,
    bank_path: &str,
    language: &str,
    code: &str,
    cancelled: &AtomicBool,
) -> Result<Vec<Segment>, Error> {
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
            language: language.to_owned(),
            bank: bank_name.to_owned(),
            code: code.to_owned(),
            offset: offset as u64,
            byte_length: byte_length as u64,
            sample_count,
            sample_rate: if sample_rate == 0 { 36000 } else { sample_rate },
            channels,
            codec: if bits_per_sample == 6 {
                "Ubisoft ADPCM 6-bit"
            } else {
                "Ubisoft ADPCM 4-bit"
            },
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
        let segments = scan_bank(
            &bytes,
            "1_2.LS0",
            "ENGLISH/1_2.LS0",
            "ENGLISH",
            "1_2",
            &cancelled,
        )
        .unwrap();
        assert_eq!(segments.len(), 2);
        assert_eq!(segments[0].offset, 0);
        assert_eq!(segments[0].id, "ENGLISH/1_2.LS0:0");
        assert_eq!(segments[0].language, "ENGLISH");
        assert_eq!(segments[0].byte_length, 103);
        assert_eq!(segments[0].sample_count, 4);
        assert_eq!(segments[0].sample_rate, 36000);
        assert_eq!(segments[0].channels, 1);
        assert_eq!(segments[1].offset, 108);
        assert_eq!(segments[1].byte_length, 103);
    }

    #[test]
    fn rejects_non_numeric_banks_and_malformed_streams() {
        assert_eq!(mission_code("1_2_3.LS0").as_deref(), Some("1_2_3"));
        assert_eq!(mission_code("Music_Common.LS0"), None);
        assert_eq!(mission_code("1_2_3.uax"), None);
        let cancelled = AtomicBool::new(false);
        assert!(
            scan_bank(
                &[0; 200],
                "1_2.LS0",
                "ENGLISH/1_2.LS0",
                "ENGLISH",
                "1_2",
                &cancelled
            )
            .unwrap()
            .is_empty()
        );
        assert!(diagnose_unrecognized_bank(&[0; 200]).contains("unsupported layout"));

        let mut truncated = make_segment();
        truncated.truncate(80);
        assert!(
            scan_bank(
                &truncated,
                "1_2.LS0",
                "ENGLISH/1_2.LS0",
                "ENGLISH",
                "1_2",
                &cancelled
            )
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

        let languages = discover_languages(game.path()).unwrap();
        assert_eq!(
            languages
                .iter()
                .map(|language| language.directory.as_str())
                .collect::<Vec<_>>(),
            ["ENGLISH", "GERMAN"]
        );
        let banks = discover_banks(game.path(), "ENGLISH").unwrap();
        assert_eq!(banks.len(), 2);
        let supported = banks.iter().find(|bank| bank.name == "1_1_0.LS0").unwrap();
        assert_eq!(supported.path, "ENGLISH/1_1_0.LS0");
        assert_eq!(supported.issue, None);
        assert_eq!(supported.segments.len(), 1);
        assert_eq!(supported.segments[0].offset, 0);
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
        read_segment(
            game.path(),
            &banks[1].path,
            0,
            banks[1].segments[0].byte_length,
        )
        .unwrap();
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
