mod banks;
pub mod convert;
pub mod decoder;
pub mod edit;
pub mod encoder;
mod error;
mod sound_map_file;
mod sounds_folder;
pub mod subtitles;
mod vgmstream;

pub use banks::{SoundKind, discover_banks, discover_banks_with_progress, discover_languages};
pub use error::Error;

use std::collections::HashMap;
use std::fs;
use std::io::{BufWriter, Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use decoder::DecodedAudio;
use sounds_folder::{find_sounds_directory, safe_child_path};

/// What scanning a game reads that's slow to read again: the banks of its
/// sound maps, and UELib's analysis of its scripts. They take most of a scan,
/// so callers can keep one cache between scans of a game's languages, and
/// start a new one for another game or to read its files again.
#[derive(Clone, Debug, Default)]
pub struct GameCache {
    /// The banks read from each sound map, such as `MAPS.SM0` or a
    /// language's `MAPS.LM0`, by the map's path relative to `Sounds`.
    pub(crate) map_banks: HashMap<String, Arc<[Bank]>>,
    pub(crate) compiled_links: Option<Arc<subtitles::CompiledLinks>>,
}

impl GameCache {
    /// Whether UELib has read the game's scripts, which is the slow part of
    /// loading subtitles the first time.
    pub fn has_read_scripts(&self) -> bool {
        self.compiled_links.is_some()
    }
}

#[derive(Clone, Debug)]
pub struct Language {
    pub directory: String,
    pub label: String,
}

/// A numeric `.LS0` bank of voice lines, or a sound map's sounds of one kind,
/// such as `MAPS.SM0`'s sound effects.
#[derive(Clone, Debug)]
pub struct Bank {
    pub name: String,
    /// The extra map whose folder holds the bank, such as `1_6_1_1kolacell`,
    /// or `None` for the language's own banks. Maps reuse bank names.
    pub map: Option<String>,
    /// The kind of sounds a sound map's bank holds, or `None` for an `.LS0`
    /// bank.
    pub sound_kind: Option<SoundKind>,
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
        /// The map's areas that play the sound.
        areas: Vec<Area>,
        /// The sound's stream, if it's one that can be replaced: a Ubisoft
        /// ADPCM stream in an `.LS0` file, such as Lambert's alarm warnings
        /// in `Music_Common.LS0`, or mono 16-bit PCM stored in the map.
        stream: Option<FileStream>,
    },
}

/// Where a map's sound's audio is, to replace it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileStream {
    /// The file, relative to `Sounds`, such as `ENGLISH/Music_Common.LS0`, or
    /// the map itself.
    pub path: String,
    /// Where the audio starts: once in an `.LS0` file, and once for each of
    /// the map's areas that plays a clip stored in it, as each area keeps a
    /// copy of its own.
    pub offsets: Vec<u64>,
    pub byte_length: u64,
    pub format: StreamFormat,
    /// The sample its loop starts at, if the game repeats it from there to
    /// its end. A replacement keeps the loop where it is.
    pub loop_start: Option<u32>,
}

/// How a stream that can be replaced is stored.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StreamFormat {
    /// Ubisoft ADPCM in an `.LS0` file, which can change length.
    UbisoftAdpcm,
    /// Mono 16-bit PCM stored in a sound map, which has to keep its length.
    Pcm16,
}

/// A segment's stream that can be replaced, from [`Segment::replaceable_stream`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReplaceableStream<'a> {
    /// The file holding it, relative to `Sounds`.
    pub path: &'a str,
    /// Where each copy of it starts.
    pub offsets: &'a [u64],
    pub byte_length: u64,
    pub format: StreamFormat,
    /// See [`FileStream::loop_start`].
    pub loop_start: Option<u32>,
}

/// One of a sound map's areas, such as `1_1_0Tbilisi`: one for each level,
/// plus `fix` and `menu`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Area {
    pub name: String,
    /// The level's mission code, such as `1_1_0`, or `None` for an area that
    /// isn't a level.
    pub code: Option<String>,
    /// The level's mission name, such as `Police Station`.
    pub mission_name: Option<String>,
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

    /// Where the segment's stream is, if it can be replaced. `bank_path` is
    /// the path of the segment's bank.
    pub fn replaceable_stream<'a>(&'a self, bank_path: &'a str) -> Option<ReplaceableStream<'a>> {
        match &self.source {
            SegmentSource::Stream {
                offset,
                byte_length,
            } => Some(ReplaceableStream {
                path: bank_path,
                offsets: std::slice::from_ref(offset),
                byte_length: *byte_length,
                format: StreamFormat::UbisoftAdpcm,
                loop_start: None,
            }),
            SegmentSource::MapSound { stream, .. } => {
                stream.as_ref().map(|stream| ReplaceableStream {
                    path: &stream.path,
                    offsets: &stream.offsets,
                    byte_length: stream.byte_length,
                    format: stream.format,
                    loop_start: stream.loop_start,
                })
            }
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

/// Decodes `segment`, like [`decode_segment`], and saves it at `destination`
/// as a WAV file.
pub fn export_segment(
    root: &Path,
    bank_path: &str,
    segment: &Segment,
    destination: &Path,
    cancelled: &AtomicBool,
) -> Result<(), Error> {
    let audio = decode_segment(root, bank_path, segment, cancelled)?;
    let written =
        fs::File::create(destination).and_then(|file| audio.write_wav(BufWriter::new(file)));
    if let Err(error) = written {
        // Don't leave a partly written file behind.
        let _ = fs::remove_file(destination);
        return Err(error.to_string().into());
    }
    Ok(())
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
