mod banks;
pub mod decoder;
mod error;
mod sounds_folder;
pub mod subtitles;
mod vgmstream;

pub use banks::{discover_banks, discover_banks_with_progress, discover_languages};
pub use error::Error;

use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use decoder::DecodedAudio;
use sounds_folder::{find_sounds_directory, safe_child_path};

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
        // A 0x30-byte stream header, then a 0x34-byte frame header.
        let mut bytes = vec![0; 0x30 + 0x34 + codes_length];
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
