//! Numeric `.LS0` banks of voice lines, such as `ENGLISH/1_1_0.LS0`, whose
//! streams are found by scanning for their headers.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use super::missions::mission_code;
use crate::sounds_folder::read_directory;
use crate::{Bank, Error, Segment, SegmentSource};

const BANK_HEADER_SIZE: usize = 0x30;
const FRAME_HEADER_SIZE: usize = 0x34;
const CODES_PER_FRAME: u32 = 1536;

/// Finds the numeric `.LS0` banks in a language folder. Subfolders aren't
/// searched: the game has none there, and a copy of a bank in one, such as a
/// backup, would make the map's references to that bank ambiguous.
pub(super) fn collect_banks(
    directory: &Path,
    relative_directory: &Path,
    output: &mut Vec<(PathBuf, PathBuf)>,
) -> Result<(), String> {
    for entry in read_directory(directory)? {
        let file_type = entry
            .file_type()
            .map_err(|error| format!("Could not inspect {}: {error}", entry.path().display()))?;
        let name = entry.file_name();
        if file_type.is_file() && is_supported_bank_name(&name.to_string_lossy()) {
            output.push((entry.path(), relative_directory.join(&name)));
        }
    }
    Ok(())
}

/// Whether `file_name` is a numeric bank, such as `1_1_0.LS0`.
pub(super) fn is_supported_bank_name(file_name: &str) -> bool {
    file_name.rsplit_once('.').is_some_and(|(stem, extension)| {
        extension.eq_ignore_ascii_case("LS0") && mission_code(stem).is_some()
    })
}

/// Scans the `.LS0` bank at `absolute_path` for streams.
pub(super) fn read_bank(
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
        sound_kind: None,
        path,
        mission_name: None,
        size: bytes.len() as u64,
        issue,
        segments,
    })
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

pub(crate) fn scan_bank(
    bytes: &[u8],
    bank_path: &str,
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
    use crate::tests::make_segment;

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
}
