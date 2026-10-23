//! Saving replaced segments: rebuilding `.LS0` banks with new streams, and
//! writing them over the game's files or into a folder of their own.
//!
//! A replacement the same length as the stream it replaces is spliced in, and
//! nothing else changes. One of another length moves the streams after it, so
//! the sound map beside the bank, its `MAPS.LM0`, is changed to match, and
//! saved with it.

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use crate::banks::scan_ls0;
use crate::encoder::{STREAM_HEADER_SIZE, StreamHeader, encode_stream};
use crate::sound_map_file::{
    OFFSET, Placement, SAMPLE_COUNT, SIZE, SIZE_AGAIN, SoundMapFile, audio_resource,
};
use crate::sounds_folder::{find_file, find_sounds_directory, safe_child_path};
use crate::{Error, Segment, StreamFormat, decoder, read_segment_with_cancel};

/// A stream to put in place of the one at `offset` in a bank.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Replacement {
    pub offset: u64,
    pub stream: Vec<u8>,
}

/// The replacements for one `.LS0` bank, whose path is relative to `Sounds`,
/// as in [`crate::Bank::path`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BankEdit {
    pub path: String,
    pub replacements: Vec<Replacement>,
}

/// Where [`save`] writes the banks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SaveTarget {
    /// Over the game's own files. Each is first copied beside itself, with
    /// `.orig` added to its name, unless that copy already exists.
    InPlace,
    /// Into this folder, as a copy of the game's `Sounds` folder holding only
    /// the saved files.
    Folder(PathBuf),
}

/// What [`save`] wrote. A file that couldn't be written is listed in
/// `failures`, and the others are still written, except those that must
/// match it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SaveReport {
    pub written: Vec<PathBuf>,
    /// The paths of the files written that the edits named, as they gave
    /// them: banks, other `.LS0` files, and maps with sounds stored in them.
    pub saved: Vec<String>,
    /// The sound maps written to match banks whose streams moved, relative
    /// to `Sounds`.
    pub maps: Vec<String>,
    /// The copies of the game's files made before they were first replaced.
    pub backups: Vec<PathBuf>,
    pub failures: Vec<String>,
}

impl SaveReport {
    pub fn is_complete(&self) -> bool {
        self.failures.is_empty()
    }
}

/// Encodes `samples` as a replacement for `segment`, a segment of the bank at
/// `bank_path` whose stream can be replaced, in the stream's format. A Ubisoft
/// ADPCM stream keeps its header.
pub fn encode_replacement(
    root: &Path,
    bank_path: &str,
    segment: &Segment,
    samples: &[i16],
    cancelled: &AtomicBool,
) -> Result<Vec<u8>, Error> {
    let stream = segment.replaceable_stream(bank_path).ok_or(CANT_REPLACE)?;
    match stream.format {
        StreamFormat::UbisoftAdpcm => {
            let header = read_segment_with_cancel(
                root,
                stream.path,
                stream.offsets[0],
                STREAM_HEADER_SIZE as u64,
                cancelled,
            )?;
            encode_stream(&StreamHeader::parse(&header)?, samples, cancelled)
        }
        StreamFormat::Pcm16 => Ok(samples
            .iter()
            .flat_map(|sample| sample.to_le_bytes())
            .collect()),
    }
}

/// Why a segment can't be replaced.
pub const CANT_REPLACE: &str = "Only dialogue, Lambert's alarm warnings, and sounds stored in the \
     sound maps as mono 16-bit PCM can be replaced.";

/// A file [`save`] writes.
struct Output {
    /// Its path relative to `Sounds`: for a file edits name, as they gave it.
    path: String,
    /// Whether edits name it, rather than it only changing to match them.
    edited: bool,
    /// The game's file.
    source: PathBuf,
    /// The game's file relative to `Sounds`, with its own spelling.
    relative: PathBuf,
    /// The game's file as it was, when saving in place, if it has to match
    /// another file, to put back if that one can't be written.
    original: Option<Vec<u8>>,
    bytes: Vec<u8>,
}

/// Files [`save`] writes together, or not at all: a sound map, if the banks'
/// streams moved, and the banks it describes. A bank whose streams stay where
/// they were is a group of its own.
struct Group {
    map: Option<Output>,
    banks: Vec<Output>,
}

/// A bank whose streams move, with their placements before and after.
struct MovedBank {
    bank: Output,
    before: Vec<Placement>,
    after: Vec<Placement>,
}

impl Group {
    /// Its files, each with whether it's the map.
    fn files(&self) -> impl Iterator<Item = (&Output, bool)> {
        let map = self.map.iter().map(|map| (map, true));
        map.chain(self.banks.iter().map(|bank| (bank, false)))
    }
}

/// Saves `edits` to `target`. Every bank and map is rebuilt and checked
/// before any is written, so a problem with one leaves all the files as they
/// were. Once writing starts, it isn't cancelled.
///
/// A map and the banks whose streams it follows to new places are written
/// together: if one of them can't be, the others are put back as they were.
///
/// Saving in place changes files the caller may have read: a
/// [`crate::GameCache`] of the game needs replacing afterwards.
pub fn save(
    root: &Path,
    edits: &[BankEdit],
    target: &SaveTarget,
    cancelled: &AtomicBool,
) -> Result<SaveReport, Error> {
    let sounds = find_sounds_directory(root)?;
    let folder = match target {
        SaveTarget::InPlace => None,
        SaveTarget::Folder(folder) => Some(export_folder(folder, &sounds)?),
    };
    let in_place = folder.is_none();
    let relative_to_sounds = |source: &Path, path: &str| {
        source
            .strip_prefix(&sounds)
            .map(Path::to_owned)
            .map_err(|_| format!("{path} isn't in the Sounds folder."))
    };

    let mut groups = Vec::new();
    // The banks whose streams move, by the map beside them.
    let mut moved: Vec<(PathBuf, Vec<MovedBank>)> = Vec::new();
    // The maps with replacements of their own.
    let mut edited_maps = Vec::new();
    for edit in edits.iter().filter(|edit| !edit.replacements.is_empty()) {
        // The resolved path, which has the file's own name however `edit`
        // spells it, so that two spellings of one bank are caught.
        let source = safe_child_path(&sounds, &edit.path)?;
        let same_file = |other: &Output| other.source == source;
        if groups
            .iter()
            .flat_map(|group: &Group| &group.banks)
            .any(same_file)
            || moved
                .iter()
                .flat_map(|(_, banks)| banks)
                .any(|moved| same_file(&moved.bank))
            || edited_maps.iter().any(same_file)
        {
            return Err(format!(
                "{} is edited more than once: its replacements must be saved together.",
                edit.path
            )
            .into());
        }
        let original = fs::read(&source)
            .map_err(|error| format!("Could not read {}: {error}", source.display()))?;
        if is_sound_map(&edit.path) {
            edited_maps.push(Output {
                path: edit.path.clone(),
                edited: true,
                relative: relative_to_sounds(&source, &edit.path)?,
                source,
                // Read again if it has to match banks too.
                original: None,
                bytes: splice_clips(original, &edit.path, &edit.replacements)?,
            });
            continue;
        }
        let rebuilt = rebuild_ls0(&original, &edit.path, &edit.replacements, cancelled)?;
        let moves = rebuilt.before != rebuilt.after;
        let bank = Output {
            path: edit.path.clone(),
            edited: true,
            relative: relative_to_sounds(&source, &edit.path)?,
            source,
            original: (moves && in_place).then_some(original),
            bytes: rebuilt.bytes,
        };
        if !moves {
            groups.push(Group {
                map: None,
                banks: vec![bank],
            });
            continue;
        }
        let map_source = map_beside(&bank.source).ok_or_else(|| no_map(&edit.path))?;
        let entry = MovedBank {
            bank,
            before: rebuilt.before,
            after: rebuilt.after,
        };
        match moved.iter_mut().find(|(map, _)| *map == map_source) {
            Some((_, banks)) => banks.push(entry),
            None => moved.push((map_source, vec![entry])),
        }
    }

    for (source, banks) in moved {
        let read = || {
            fs::read(&source)
                .map_err(|error| format!("Could not read {}: {error}", source.display()))
        };
        // The map with its own replacements, if it has any.
        let edited = edited_maps
            .iter()
            .position(|map| map.source == source)
            .map(|index| edited_maps.swap_remove(index));
        let (mut bytes, original, path, relative, edited) = match edited {
            Some(map) => (
                map.bytes,
                in_place.then(read).transpose()?,
                map.path,
                map.relative,
                true,
            ),
            None => {
                let bytes = read()?;
                let relative = relative_to_sounds(&source, "MAPS.LM0")?;
                let path = relative.to_string_lossy().replace('\\', "/");
                (
                    bytes.clone(),
                    in_place.then_some(bytes),
                    path,
                    relative,
                    false,
                )
            }
        };
        let names: Vec<_> = banks
            .iter()
            .map(|moved| {
                moved
                    .bank
                    .relative
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
            })
            .collect();
        let streams: Vec<_> = banks
            .iter()
            .zip(&names)
            .map(|(moved, name)| (name.as_ref(), &moved.before[..], &moved.after[..]))
            .collect();
        // A map that has none of the banks' streams, and no replacements of
        // its own, stays as it is.
        let changed = patch_map(&mut bytes, &path, &streams)? > 0 || edited;
        drop(streams);
        groups.push(Group {
            map: changed.then_some(Output {
                path,
                edited,
                source,
                relative,
                original,
                bytes,
            }),
            banks: banks.into_iter().map(|moved| moved.bank).collect(),
        });
    }
    groups.extend(edited_maps.into_iter().map(|map| Group {
        map: Some(map),
        banks: Vec::new(),
    }));
    if cancelled.load(Ordering::Relaxed) {
        return Err(Error::Cancelled);
    }

    let mut report = SaveReport::default();
    for group in &groups {
        write_group(group, folder.as_deref(), &mut report);
    }
    Ok(report)
}

/// Whether the file at `path` is a sound map, `MAPS.SM0` or `MAPS.LM0`.
pub fn is_sound_map(path: &str) -> bool {
    path.rsplit_once('.').is_some_and(|(_, extension)| {
        extension.eq_ignore_ascii_case("SM0") || extension.eq_ignore_ascii_case("LM0")
    })
}

/// `map`, the sound map at `map_path`, with `replacements` in place of clips
/// stored in it. Each has to be the same length as its clip: another length
/// would move everything after it in the map.
pub(crate) fn splice_clips(
    mut map: Vec<u8>,
    map_path: &str,
    replacements: &[Replacement],
) -> Result<Vec<u8>, Error> {
    let file = SoundMapFile::parse(&map).map_err(|error| format!("{map_path} {error}."))?;
    let clips: HashMap<u64, u64> = file
        .areas()
        .iter()
        .flat_map(|area| file.stored_clips(area))
        .filter(|clip| clip.channels == 1)
        .map(|clip| (clip.offset, clip.length))
        .collect();
    let mut replaced = HashSet::new();
    for replacement in replacements {
        let offset = replacement.offset;
        let Some(&length) = clips.get(&offset) else {
            return Err(
                format!("{map_path} has no sound stored at offset {offset} to replace.").into(),
            );
        };
        if !replaced.insert(offset) {
            return Err(format!(
                "The sound at offset {offset} in {map_path} is replaced more than once."
            )
            .into());
        }
        if replacement.stream.len() as u64 != length {
            return Err(format!(
                "The replacement for the sound at offset {offset} in {map_path} is {} bytes \
                 long, but a sound stored in a map has to keep its length, {length} bytes.",
                replacement.stream.len()
            )
            .into());
        }
        let start = offset as usize;
        map[start..start + replacement.stream.len()].copy_from_slice(&replacement.stream);
    }
    Ok(map)
}

/// The sound map beside the bank at `source`, which describes it.
fn map_beside(source: &Path) -> Option<PathBuf> {
    find_file(source.parent()?, "MAPS.LM0").ok()
}

fn no_map(bank_path: &str) -> String {
    format!(
        "{bank_path} has a replacement of another length, which needs the sound map beside it, \
         MAPS.LM0, but there isn't one."
    )
}

/// Writes `group`, files that must match each other, over the game's files
/// or into `folder`. If one can't be written, those written before it are put
/// back as they were.
fn write_group(group: &Group, folder: Option<&Path>, report: &mut SaveReport) {
    let files: Vec<_> = group.files().collect();
    let others = |failed: &Output| {
        let others: Vec<_> = files
            .iter()
            .filter(|(output, _)| output.path != failed.path)
            .map(|(output, _)| output.path.as_str())
            .collect();
        match others.as_slice() {
            [] => String::new(),
            [other] => format!(" {other} wasn't saved either, as it must match it."),
            others => format!(
                " {} weren't saved either, as they must match it.",
                others.join(", ")
            ),
        }
    };

    // What was at each destination, to put back if another file fails. Only
    // a file that has to match another is ever put back, and saving in place
    // keeps the original of each of those.
    let mut previous = Vec::with_capacity(files.len());
    for &(output, _) in &files {
        let (destination, existing) = match folder {
            Some(folder) => {
                let destination = folder.join(&output.relative);
                let existing = match (files.len() > 1).then(|| fs::read(&destination)) {
                    None => None,
                    Some(Ok(bytes)) => Some(Cow::Owned(bytes)),
                    Some(Err(error)) if error.kind() == io::ErrorKind::NotFound => None,
                    Some(Err(error)) => {
                        report.failures.push(format!(
                            "Could not read {}: {error}.{}",
                            destination.display(),
                            others(output)
                        ));
                        return;
                    }
                };
                (destination, existing)
            }
            None => {
                match back_up(&output.source) {
                    Ok(backup) => report.backups.extend(backup),
                    Err(error) => {
                        report.failures.push(format!(
                            "{} wasn't saved, as it couldn't be backed up: {error}.{}",
                            output.path,
                            others(output)
                        ));
                        return;
                    }
                }
                debug_assert!(files.len() == 1 || output.original.is_some());
                let original = output.original.as_deref().map(Cow::Borrowed);
                (output.source.clone(), original)
            }
        };
        previous.push((destination, existing));
    }

    for (index, &(output, _)) in files.iter().enumerate() {
        let destination = &previous[index].0;
        let Err(error) = write_atomically(destination, &output.bytes) else {
            continue;
        };
        let mut failure = format!(
            "Could not write {}: {error}.{}",
            destination.display(),
            others(output)
        );
        for ((written, _), (destination, existing)) in files.iter().zip(&previous).take(index) {
            let restored = match existing {
                Some(bytes) => write_atomically(destination, bytes),
                None => fs::remove_file(destination),
            };
            if let Err(error) = restored {
                failure.push_str(&format!(
                    " {} couldn't be put back as it was, so it doesn't match the others: {error}.",
                    written.path
                ));
            }
        }
        report.failures.push(failure);
        return;
    }
    for ((output, _), (destination, _)) in files.into_iter().zip(previous) {
        report.written.push(destination);
        if output.edited {
            report.saved.push(output.path.clone());
        } else {
            report.maps.push(output.path.clone());
        }
    }
}

/// The `Sounds` folder to export into inside `folder`, which mustn't be the
/// game's own.
fn export_folder(folder: &Path, sounds: &Path) -> Result<PathBuf, Error> {
    let folder = folder
        .canonicalize()
        .map_err(|error| format!("Could not open {}: {error}", folder.display()))?;
    if sounds.parent() == Some(folder.as_path()) || folder.starts_with(sounds) {
        return Err("Choose a folder outside the game's, so its files aren't replaced.".into());
    }
    Ok(folder.join("Sounds"))
}

/// Copies `path` beside itself as `<name>.orig`, unless that exists, and
/// returns the copy's path if it made one. The bank scan ignores the copy,
/// as its name doesn't end in `.LS0`.
fn back_up(path: &Path) -> io::Result<Option<PathBuf>> {
    let mut name = path.file_name().unwrap_or_default().to_owned();
    name.push(".orig");
    let backup = path.with_file_name(name);
    if backup.exists() {
        return Ok(None);
    }
    let mut temporary = backup.clone().into_os_string();
    temporary.push(".tmp");
    let temporary = PathBuf::from(temporary);
    let copied = fs::copy(path, &temporary).and_then(|_| fs::rename(&temporary, &backup));
    if copied.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    copied.map(|()| Some(backup))
}

/// Writes `bytes` to a new file beside `path`, then renames it over `path`,
/// so that `path` is never left half-written.
fn write_atomically(path: &Path, bytes: &[u8]) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut temporary = path.as_os_str().to_owned();
    temporary.push(format!(".{}.tmp", std::process::id()));
    let temporary = PathBuf::from(temporary);
    let written = fs::File::create(&temporary)
        .and_then(|mut file| {
            file.write_all(bytes)?;
            file.sync_all()
        })
        .and_then(|()| fs::rename(&temporary, path));
    if written.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    written
}

impl Placement {
    fn of(segment: &Segment) -> Option<Self> {
        let (offset, length) = segment.stream_range()?;
        Some(Self {
            offset,
            length,
            sample_count: segment.sample_count,
        })
    }
}

/// A bank rebuilt with its replacements.
#[derive(Debug)]
pub(crate) struct RebuiltBank {
    pub(crate) bytes: Vec<u8>,
    /// Every stream's placement in the original, in order.
    pub(crate) before: Vec<Placement>,
    /// The same streams' placements in `bytes`.
    pub(crate) after: Vec<Placement>,
}

/// `original`, the `.LS0` bank at `bank_path`, with `replacements` in place
/// of its streams.
///
/// A replacement the same length as its stream is spliced in. One of another
/// length is followed by fresh padding to the next 4 bytes, where the game's
/// streams start, and the streams after it move. The bytes between streams
/// are kept, except around a stream that changes length: there, they must be
/// only the padding, or the bank isn't laid out as this expects.
///
/// The result is checked by scanning it again: it must have the same
/// streams, at the places expected, and the new ones must decode.
pub(crate) fn rebuild_ls0(
    original: &[u8],
    bank_path: &str,
    replacements: &[Replacement],
    cancelled: &AtomicBool,
) -> Result<RebuiltBank, Error> {
    let segments = scan_ls0(original, bank_path, cancelled)?;
    let before: Vec<Placement> = segments.iter().filter_map(Placement::of).collect();
    let mut streams: Vec<Option<&[u8]>> = vec![None; before.len()];
    for replacement in replacements {
        let Some(index) = before
            .iter()
            .position(|placement| placement.offset == replacement.offset)
        else {
            return Err(format!(
                "{bank_path} has no stream at offset {} to replace.",
                replacement.offset
            )
            .into());
        };
        if streams[index].is_some() {
            return Err(format!("{} is replaced more than once.", segments[index].id).into());
        }
        streams[index] = Some(&replacement.stream);
    }

    let mut bytes = Vec::with_capacity(original.len());
    let mut after = Vec::with_capacity(before.len());
    let first = before
        .first()
        .map_or(original.len(), |first| first.offset as usize);
    bytes.extend_from_slice(&original[..first]);
    for (index, placement) in before.iter().enumerate() {
        let start = placement.offset as usize;
        let end = start + placement.length as usize;
        let next = before
            .get(index + 1)
            .map_or(original.len(), |next| next.offset as usize);
        let offset = bytes.len() as u64;
        let Some(stream) = streams[index] else {
            bytes.extend_from_slice(&original[start..next]);
            after.push(Placement {
                offset,
                ..*placement
            });
            continue;
        };
        let id = &segments[index].id;
        let header = StreamHeader::parse(stream)
            .map_err(|error| format!("The replacement for {id} isn't a stream: {error}"))?;
        bytes.extend_from_slice(stream);
        if stream.len() == end - start {
            bytes.extend_from_slice(&original[end..next]);
        } else {
            if !can_change_length(original.len(), &before, index) {
                return Err(unexpected_layout(id, bank_path).into());
            }
            bytes.resize(bytes.len().next_multiple_of(4), 0);
        }
        after.push(Placement {
            offset,
            length: stream.len() as u64,
            sample_count: header.sample_count(),
        });
    }

    let layout = |segments: &[Segment]| {
        segments
            .iter()
            .map(|segment| (Placement::of(segment), segment.codec.clone()))
            .collect::<Vec<_>>()
    };
    let expected: Vec<_> = after
        .iter()
        .zip(&segments)
        .map(|(placement, segment)| (Some(*placement), segment.codec.clone()))
        .collect();
    if layout(&scan_ls0(&bytes, bank_path, cancelled)?) != expected {
        return Err(format!(
            "Saving {bank_path} would change streams that weren't replaced, so it wasn't saved."
        )
        .into());
    }
    for (index, placement) in after.iter().enumerate() {
        if streams[index].is_none() {
            continue;
        }
        let start = placement.offset as usize;
        let stream = &bytes[start..start + placement.length as usize];
        decoder::decode(stream, placement.sample_count, cancelled).map_err(
            |error| match error {
                Error::Cancelled => Error::Cancelled,
                Error::Failed(message) => format!(
                    "The replacement for {} doesn't decode: {message}",
                    segments[index].id
                )
                .into(),
            },
        )?;
    }
    Ok(RebuiltBank {
        bytes,
        before,
        after,
    })
}

/// Whether the stream at `index` in `streams`, the streams of a bank of
/// `bank_length` bytes, can change length: it starts on 4 bytes, and only the
/// padding to the next 4 bytes follows it.
fn can_change_length(bank_length: usize, streams: &[Placement], index: usize) -> bool {
    let stream = streams[index];
    let end = (stream.offset + stream.length) as usize;
    let next = streams
        .get(index + 1)
        .map_or(bank_length, |next| next.offset as usize);
    stream.offset.is_multiple_of(4)
        && next - end < 4
        && (next == bank_length || next.is_multiple_of(4))
}

fn unexpected_layout(segment_id: &str, bank_path: &str) -> String {
    format!(
        "{segment_id} can't change length: the bytes around it in {bank_path} aren't laid out \
         as expected, so it can't be moved."
    )
}

/// Checks that the stream at `offset` in the `.LS0` bank at `bank_path` can be
/// saved at another length: that only padding follows it, and that the sound
/// map beside the bank, which changes too, points only at the bank's streams.
pub fn check_length_change(
    root: &Path,
    bank_path: &str,
    offset: u64,
    cancelled: &AtomicBool,
) -> Result<(), Error> {
    let sounds = find_sounds_directory(root)?;
    let source = safe_child_path(&sounds, bank_path)?;
    let bank = fs::read(&source)
        .map_err(|error| format!("Could not read {}: {error}", source.display()))?;
    let segments = scan_ls0(&bank, bank_path, cancelled)?;
    let streams: Vec<_> = segments.iter().filter_map(Placement::of).collect();
    let index = streams
        .iter()
        .position(|stream| stream.offset == offset)
        .ok_or_else(|| format!("{bank_path} has no stream at offset {offset}."))?;
    if !can_change_length(bank.len(), &streams, index) {
        return Err(unexpected_layout(&segments[index].id, bank_path).into());
    }
    let map_source = map_beside(&source).ok_or_else(|| no_map(bank_path))?;
    let map = fs::read(&map_source)
        .map_err(|error| format!("Could not read {}: {error}", map_source.display()))?;
    let map_path = map_source
        .strip_prefix(&sounds)
        .unwrap_or(&map_source)
        .to_string_lossy()
        .replace('\\', "/");
    let bank_file = source.file_name().unwrap_or_default().to_string_lossy();
    resources_in(&map, &map_path, &[(&bank_file, &streams)])?;
    Ok(())
}

/// A bank's file name, and its streams' placements before and after it was
/// rebuilt, in the same order.
pub(crate) type MovedStreams<'a> = (&'a str, &'a [Placement], &'a [Placement]);

/// Changes `map`, the sound map at `map_path`, so that its resources for each
/// bank's streams follow them to their new placements, in every area.
/// Returns how many resources changed.
///
/// Every audio resource in one of the banks must be at one of its streams,
/// with its length and sample count. Otherwise the map refers to something
/// the bank's scan didn't find, which could be moved from under it.
pub(crate) fn patch_map(
    map: &mut [u8],
    map_path: &str,
    banks: &[MovedStreams],
) -> Result<usize, Error> {
    let before: Vec<_> = banks
        .iter()
        .map(|&(name, before, _)| (name, before))
        .collect();
    let mut changed = 0;
    for (resource, bank, stream) in resources_in(map, map_path, &before)? {
        let (bank_file, before, after) = banks[bank];
        let placement = after[stream];
        if placement == before[stream] {
            continue;
        }
        let too_large = || format!("{bank_file} would be too large for {map_path} to describe.");
        let offset = u32::try_from(placement.offset).map_err(|_| too_large())?;
        let length = u32::try_from(placement.length).map_err(|_| too_large())?;
        for (field, value) in [
            (SIZE, length),
            (OFFSET, offset),
            (SAMPLE_COUNT, placement.sample_count),
            (SIZE_AGAIN, length),
        ] {
            let position = resource + field;
            map[position..position + 4].copy_from_slice(&value.to_le_bytes());
        }
        changed += 1;
    }
    // Read the result back, as the game would.
    let after: Vec<_> = banks
        .iter()
        .map(|&(name, _, after)| (name, after))
        .collect();
    resources_in(map, map_path, &after)?;
    Ok(changed)
}

/// The audio resources in `map` in each of `banks`, given by file name and
/// streams, as the offset of each resource, the index of its bank, and the
/// index of the stream it's at.
fn resources_in(
    map: &[u8],
    map_path: &str,
    banks: &[(&str, &[Placement])],
) -> Result<Vec<(usize, usize, usize)>, Error> {
    let file = SoundMapFile::parse(map).map_err(|error| format!("{map_path} {error}."))?;
    let mut found = Vec::new();
    for area in file.areas() {
        for resource in file.resources(area) {
            let Some(audio) = audio_resource(map, resource) else {
                continue;
            };
            let Some(bank) = banks.iter().position(|(bank_file, _)| {
                bank_file.as_bytes().eq_ignore_ascii_case(audio.file_name)
            }) else {
                continue;
            };
            let (bank_file, streams) = banks[bank];
            // A looping resource's sample count and size again are split in
            // two, so `patch_map` can't write them. The game's maps have none
            // in a bank.
            if audio.loop_start.is_some() {
                return Err(format!(
                    "{map_path} has a looping sound in {bank_file}, so its streams can't be moved."
                )
                .into());
            }
            let stream = audio
                .placement
                .and_then(|placement| streams.iter().position(|stream| *stream == placement));
            let Some(stream) = stream else {
                return Err(format!(
                    "{map_path} has a sound in {bank_file} that isn't one of the streams found \
                     in it, so its streams can't be moved."
                )
                .into());
            };
            found.push((resource, bank, stream));
        }
    }
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SegmentSource;
    use crate::sound_map_file::tests::write_u32;
    use crate::sound_map_file::{AREA_SIZE, AUDIO, RESOURCE_SIZE};

    fn template() -> StreamHeader {
        StreamHeader::parse(&crate::tests::make_segment()).unwrap()
    }

    fn tone(length: usize, amplitude: f64) -> Vec<i16> {
        (0..length)
            .map(|index| ((index as f64 / 20.0).sin() * amplitude) as i16)
            .collect()
    }

    fn stream(samples: &[i16]) -> Vec<u8> {
        encode_stream(&template(), samples, &AtomicBool::new(false)).unwrap()
    }

    /// A bank of streams of the given lengths, each padded to 4 bytes, with
    /// the offset of each.
    fn bank(lengths: &[usize]) -> (Vec<u8>, Vec<u64>) {
        let mut bytes = Vec::new();
        let mut offsets = Vec::new();
        for &length in lengths {
            offsets.push(bytes.len() as u64);
            bytes.extend(stream(&tone(length, 4000.0)));
            // Padding the game's banks have, which isn't always zero.
            while bytes.len() % 4 != 0 {
                bytes.push(0x5A);
            }
        }
        (bytes, offsets)
    }

    /// A game folder with `ENGLISH/1_1_0.LS0` and `ENGLISH/1_1_1.LS0`.
    fn game() -> (tempfile::TempDir, Vec<u8>, Vec<u64>) {
        let game = tempfile::tempdir().unwrap();
        let english = game.path().join("Sounds/ENGLISH");
        fs::create_dir_all(&english).unwrap();
        let (bytes, offsets) = bank(&[3000, 5000, 1000]);
        fs::write(english.join("1_1_0.LS0"), &bytes).unwrap();
        fs::write(english.join("1_1_1.LS0"), &bytes).unwrap();
        (game, bytes, offsets)
    }

    fn edit(path: &str, offset: u64, length: usize) -> BankEdit {
        BankEdit {
            path: path.to_owned(),
            replacements: vec![Replacement {
                offset,
                stream: stream(&tone(length, 9000.0)),
            }],
        }
    }

    fn segment(offset: u64, byte_length: u64, sample_count: u32) -> Segment {
        Segment {
            id: format!("ENGLISH/1_1_0.LS0:{offset}"),
            source: SegmentSource::Stream {
                offset,
                byte_length,
            },
            name: None,
            sample_count,
            sample_rate: 36000,
            channels: 1,
            codec: "Ubisoft ADPCM 4-bit".to_owned(),
            segment: 0,
        }
    }

    #[test]
    fn replacing_changes_only_the_stream() {
        let (original, offsets) = bank(&[3000, 5000, 1000]);
        let cancelled = AtomicBool::new(false);
        let replacement = stream(&tone(5000, 9000.0));
        let rebuilt = rebuild_ls0(
            &original,
            "ENGLISH/1_1_0.LS0",
            &[Replacement {
                offset: offsets[1],
                stream: replacement.clone(),
            }],
            &cancelled,
        )
        .unwrap();
        assert_eq!(rebuilt.before, rebuilt.after);
        let rebuilt = rebuilt.bytes;
        let start = offsets[1] as usize;
        let end = start + replacement.len();
        assert_eq!(rebuilt.len(), original.len());
        assert_eq!(rebuilt[..start], original[..start]);
        assert_eq!(rebuilt[start..end], replacement);
        assert_eq!(rebuilt[end..], original[end..]);
    }

    #[test]
    fn replacements_must_be_for_a_stream() {
        let (original, offsets) = bank(&[3000, 5000]);
        let cancelled = AtomicBool::new(false);
        let rebuild = |offset, length| {
            rebuild_ls0(
                &original,
                "ENGLISH/1_1_0.LS0",
                &edit("", offset, length).replacements,
                &cancelled,
            )
            .unwrap_err()
            .to_string()
        };
        assert!(rebuild(offsets[1] + 4, 5000).contains("no stream at offset"));

        let twice = [edit("", offsets[0], 3000), edit("", offsets[0], 3000)]
            .map(|edit| edit.replacements.into_iter().next().unwrap());
        let error = rebuild_ls0(&original, "ENGLISH/1_1_0.LS0", &twice, &cancelled).unwrap_err();
        assert!(error.to_string().contains("more than once"), "{error}");
    }

    #[test]
    fn saving_in_place_backs_up_the_original_once() {
        let (game, original, offsets) = game();
        let cancelled = AtomicBool::new(false);
        let path = game.path().join("Sounds/ENGLISH/1_1_0.LS0");
        let backup = game.path().join("Sounds/ENGLISH/1_1_0.LS0.orig");

        let first = edit("ENGLISH/1_1_0.LS0", offsets[0], 3000);
        let report = save(game.path(), &[first], &SaveTarget::InPlace, &cancelled).unwrap();
        assert!(report.is_complete());
        assert_eq!(report.written, [path.canonicalize().unwrap()]);
        assert_eq!(report.backups, [backup.canonicalize().unwrap()]);
        assert_eq!(fs::read(&backup).unwrap(), original);
        let saved = fs::read(&path).unwrap();
        assert_ne!(saved, original);

        // A second save keeps the first backup, of the game's own file.
        let second = edit("ENGLISH/1_1_0.LS0", offsets[2], 1000);
        let report = save(game.path(), &[second], &SaveTarget::InPlace, &cancelled).unwrap();
        assert!(report.backups.is_empty());
        assert_eq!(fs::read(&backup).unwrap(), original);
        // The first replacement is still there.
        assert_eq!(
            fs::read(&path).unwrap()[..offsets[1] as usize],
            saved[..offsets[1] as usize]
        );
        // No temporary files are left behind.
        let names: Vec<_> = fs::read_dir(game.path().join("Sounds/ENGLISH"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        assert_eq!(names.len(), 3, "{names:?}");
    }

    #[test]
    fn saving_to_a_folder_leaves_the_game_alone() {
        let (game, original, offsets) = game();
        let output = tempfile::tempdir().unwrap();
        let cancelled = AtomicBool::new(false);
        let report = save(
            game.path(),
            &[edit("ENGLISH/1_1_1.LS0", offsets[1], 5000)],
            &SaveTarget::Folder(output.path().to_owned()),
            &cancelled,
        )
        .unwrap();
        let exported = output
            .path()
            .canonicalize()
            .unwrap()
            .join("Sounds/ENGLISH/1_1_1.LS0");
        assert_eq!(report.written, std::slice::from_ref(&exported));
        assert!(report.backups.is_empty());
        assert_eq!(fs::read(exported).unwrap().len(), original.len());
        for name in ["1_1_0.LS0", "1_1_1.LS0"] {
            let path = game.path().join("Sounds/ENGLISH").join(name);
            assert_eq!(fs::read(path).unwrap(), original);
        }
        assert!(!game.path().join("Sounds/ENGLISH/1_1_1.LS0.orig").exists());
    }

    #[test]
    fn nothing_is_written_if_any_bank_fails() {
        let (game, original, offsets) = game();
        let cancelled = AtomicBool::new(false);
        let edits = [
            edit("ENGLISH/1_1_0.LS0", offsets[0], 3000),
            edit("ENGLISH/1_1_1.LS0", offsets[0] + 4, 3000),
        ];
        assert!(save(game.path(), &edits, &SaveTarget::InPlace, &cancelled).is_err());
        let folder = game.path().join("Sounds/ENGLISH");
        assert_eq!(fs::read(folder.join("1_1_0.LS0")).unwrap(), original);
        assert!(!folder.join("1_1_0.LS0.orig").exists());

        cancelled.store(true, Ordering::Relaxed);
        let edits = [edit("ENGLISH/1_1_0.LS0", offsets[0], 3000)];
        assert_eq!(
            save(game.path(), &edits, &SaveTarget::InPlace, &cancelled),
            Err(Error::Cancelled)
        );
        assert_eq!(fs::read(folder.join("1_1_0.LS0")).unwrap(), original);
    }

    #[test]
    fn failures_are_reported_and_the_rest_written() {
        let (game, _, offsets) = game();
        let output = tempfile::tempdir().unwrap();
        // A file where the export needs a folder.
        fs::create_dir_all(output.path().join("Sounds")).unwrap();
        fs::write(output.path().join("Sounds/ENGLISH"), b"").unwrap();
        fs::create_dir_all(game.path().join("Sounds/GERMAN")).unwrap();
        fs::copy(
            game.path().join("Sounds/ENGLISH/1_1_0.LS0"),
            game.path().join("Sounds/GERMAN/1_1_0.LS0"),
        )
        .unwrap();
        let edits = [
            edit("ENGLISH/1_1_0.LS0", offsets[0], 3000),
            edit("GERMAN/1_1_0.LS0", offsets[0], 3000),
        ];
        let report = save(
            game.path(),
            &edits,
            &SaveTarget::Folder(output.path().to_owned()),
            &AtomicBool::new(false),
        )
        .unwrap();
        assert!(!report.is_complete());
        assert_eq!(report.failures.len(), 1);
        assert!(
            report.failures[0].contains("ENGLISH"),
            "{:?}",
            report.failures
        );
        assert_eq!(report.written.len(), 1);
        assert!(report.written[0].ends_with("Sounds/GERMAN/1_1_0.LS0"));
        assert_eq!(report.saved, ["GERMAN/1_1_0.LS0"]);
    }

    #[test]
    fn a_bank_can_only_be_edited_once() {
        let (game, original, offsets) = game();
        let cancelled = AtomicBool::new(false);
        let mut edits = vec![
            edit("ENGLISH/1_1_0.LS0", offsets[0], 3000),
            edit("ENGLISH/./1_1_0.LS0", offsets[2], 1000),
        ];
        let error = save(game.path(), &edits, &SaveTarget::InPlace, &cancelled).unwrap_err();
        assert!(error.to_string().contains("more than once"), "{error}");
        let path = game.path().join("Sounds/ENGLISH/1_1_0.LS0");
        assert_eq!(fs::read(&path).unwrap(), original);

        // On a disk that ignores case, another spelling is the same bank.
        if game.path().join("Sounds/english/1_1_0.ls0").exists() {
            edits[1].path = "english/1_1_0.ls0".to_owned();
            assert!(save(game.path(), &edits, &SaveTarget::InPlace, &cancelled).is_err());
            assert_eq!(fs::read(&path).unwrap(), original);
        }
    }

    #[test]
    fn exports_use_the_games_names() {
        let (game, _, offsets) = game();
        if !game.path().join("Sounds/english/1_1_0.ls0").exists() {
            // Only another spelling on a disk that ignores case can differ.
            return;
        }
        let output = tempfile::tempdir().unwrap();
        let report = save(
            game.path(),
            &[edit("english/1_1_0.ls0", offsets[0], 3000)],
            &SaveTarget::Folder(output.path().to_owned()),
            &AtomicBool::new(false),
        )
        .unwrap();
        let names = |path: &Path| {
            path.components()
                .rev()
                .take(2)
                .map(|part| part.as_os_str().to_string_lossy().into_owned())
                .collect::<Vec<_>>()
        };
        assert_eq!(names(&report.written[0]), ["1_1_0.LS0", "ENGLISH"]);
        let folder = output.path().join("Sounds");
        let written: Vec<_> = fs::read_dir(&folder)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(written, ["ENGLISH"]);
    }

    #[test]
    fn edits_without_replacements_are_skipped() {
        let (game, original, _) = game();
        let edits = [BankEdit {
            path: "ENGLISH/1_1_0.LS0".to_owned(),
            replacements: Vec::new(),
        }];
        let report = save(
            game.path(),
            &edits,
            &SaveTarget::InPlace,
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(report, SaveReport::default());
        let folder = game.path().join("Sounds/ENGLISH");
        assert_eq!(fs::read(folder.join("1_1_0.LS0")).unwrap(), original);
        assert!(!folder.join("1_1_0.LS0.orig").exists());
    }

    #[test]
    fn exports_must_be_outside_the_game() {
        let (game, _, offsets) = game();
        let cancelled = AtomicBool::new(false);
        let edits = [edit("ENGLISH/1_1_0.LS0", offsets[0], 3000)];
        for folder in [
            game.path().to_owned(),
            game.path().join("Sounds"),
            game.path().join("Sounds/ENGLISH"),
        ] {
            let error =
                save(game.path(), &edits, &SaveTarget::Folder(folder), &cancelled).unwrap_err();
            assert!(error.to_string().contains("outside the game"), "{error}");
        }
    }

    #[test]
    fn replacements_are_encoded_with_the_streams_header() {
        let (game, original, offsets) = game();
        let cancelled = AtomicBool::new(false);
        let length = (offsets[1] - offsets[0]) as usize;
        let byte_length = crate::banks::scan_ls0(&original, "ENGLISH/1_1_0.LS0", &cancelled)
            .unwrap()[0]
            .stream_range()
            .unwrap()
            .1;
        assert!(byte_length as usize <= length);
        let target = segment(offsets[0], byte_length, 3000);
        let samples = tone(3000, 7000.0);
        let stream = encode_replacement(
            game.path(),
            "ENGLISH/1_1_0.LS0",
            &target,
            &samples,
            &cancelled,
        )
        .unwrap();
        assert_eq!(stream.len() as u64, byte_length);
        assert_eq!(stream[..STREAM_HEADER_SIZE], original[..STREAM_HEADER_SIZE]);
    }

    fn placements(bank: &[u8]) -> Vec<Placement> {
        scan_ls0(bank, "ENGLISH/1_1_0.LS0", &AtomicBool::new(false))
            .unwrap()
            .iter()
            .filter_map(Placement::of)
            .collect()
    }

    /// A map whose areas each have audio resources for the given streams, by
    /// file name, after a container whose leftover file name is the first's.
    fn map_of(areas: &[&[(&str, Placement)]]) -> Vec<u8> {
        let mut data = vec![0; 0x0c + areas.len() * AREA_SIZE];
        for (position, value) in [(0, 7), (4, 0x0c), (8, areas.len() as u32)] {
            write_u32(&mut data, position, value);
        }
        for (index, resources) in areas.iter().enumerate() {
            let header = data.len();
            write_u32(&mut data, 0x0c + index * AREA_SIZE + 8, header as u32);
            data.resize(header + 0x24, 0);
            for (field, value) in [(4, 0x24), (0xc, 0x24), (0x10, resources.len() + 1)] {
                write_u32(&mut data, header + field, value as u32);
            }
            let container = data.len();
            data.resize(container + RESOURCE_SIZE, 0);
            write_u32(&mut data, container + 4, 10);
            write_u32(&mut data, container + OFFSET, 7);
            let name = resources[0].0.as_bytes();
            data[container + 80..container + 80 + name.len()].copy_from_slice(name);
            for (name, placement) in *resources {
                let resource = data.len();
                data.resize(resource + RESOURCE_SIZE, 0);
                for (field, value) in [
                    (4, AUDIO),
                    (8, 1),
                    (SIZE, placement.length as u32),
                    (OFFSET, placement.offset as u32),
                    (SAMPLE_COUNT, placement.sample_count),
                    (SIZE_AGAIN, placement.length as u32),
                    (68, 36_000),
                ] {
                    write_u32(&mut data, resource + field, value);
                }
                data[resource + 80..resource + 80 + name.len()].copy_from_slice(name.as_bytes());
            }
        }
        data
    }

    /// [`game`], with a map in two areas: the first has every stream of
    /// both banks, and the second the first two streams of `1_1_0.LS0`.
    fn game_with_map() -> (tempfile::TempDir, Vec<u8>, Vec<u64>, Vec<u8>) {
        let (game, bank, offsets) = game();
        let streams = placements(&bank);
        let every: Vec<_> = streams
            .iter()
            .flat_map(|stream| [("1_1_0.LS0", *stream), ("1_1_1.LS0", *stream)])
            .collect();
        let first_two = [("1_1_0.LS0", streams[0]), ("1_1_0.LS0", streams[1])];
        let map = map_of(&[&every, &first_two]);
        fs::write(game.path().join("Sounds/ENGLISH/MAPS.LM0"), &map).unwrap();
        (game, bank, offsets, map)
    }

    #[test]
    fn a_stream_of_another_length_moves_the_ones_after_it() {
        let (original, offsets) = bank(&[3000, 5000, 1000]);
        let before = placements(&original);
        let cancelled = AtomicBool::new(false);
        for length in [7000, 2000] {
            let replacement = stream(&tone(length, 9000.0));
            let rebuilt = rebuild_ls0(
                &original,
                "ENGLISH/1_1_0.LS0",
                &[Replacement {
                    offset: offsets[1],
                    stream: replacement.clone(),
                }],
                &cancelled,
            )
            .unwrap();
            assert_eq!(rebuilt.before, before);
            assert_eq!(rebuilt.after, placements(&rebuilt.bytes));
            let [first, second, third] = rebuilt.after[..] else {
                panic!("{:?}", rebuilt.after);
            };
            assert_eq!(first, before[0]);
            assert_eq!(second.offset, before[1].offset);
            assert_eq!(second.length, replacement.len() as u64);
            assert_eq!(second.sample_count, length as u32);
            // Fresh padding, then the last stream as it was, with what
            // followed it.
            assert_eq!(
                third.offset,
                (second.offset + second.length).next_multiple_of(4)
            );
            let bytes = &rebuilt.bytes;
            assert!(
                bytes[(second.offset + second.length) as usize..third.offset as usize]
                    .iter()
                    .all(|byte| *byte == 0)
            );
            assert_eq!(
                bytes[third.offset as usize..],
                original[before[2].offset as usize..]
            );
            assert_eq!(
                bytes[..offsets[1] as usize],
                original[..offsets[1] as usize]
            );
        }
    }

    #[test]
    fn streams_only_change_length_within_padding() {
        let (mut original, offsets) = bank(&[3000, 5000]);
        // Bytes this doesn't know between the streams.
        let gap = offsets[1] as usize;
        original.splice(gap..gap, [0x5A; 8]);
        let second = placements(&original)[1].offset;
        let cancelled = AtomicBool::new(false);
        let rebuild = |offset, length| {
            rebuild_ls0(
                &original,
                "ENGLISH/1_1_0.LS0",
                &edit("", offset, length).replacements,
                &cancelled,
            )
        };
        let error = rebuild(offsets[0], 4000).unwrap_err();
        assert!(
            error.to_string().contains("aren't laid out as expected"),
            "{error}"
        );
        // The first stream can stay the same length, and the last one can
        // change, as only padding follows it.
        assert!(rebuild(offsets[0], 3000).is_ok());
        let rebuilt = rebuild(second, 6000).unwrap();
        assert_eq!(
            rebuilt.bytes[..second as usize],
            original[..second as usize]
        );
        assert_eq!(rebuilt.bytes.len() % 4, 0);
    }

    #[test]
    fn patching_a_map_moves_the_banks_resources_in_every_area() {
        let (original, offsets) = bank(&[3000, 5000, 1000]);
        let before = placements(&original);
        let map = map_of(&[
            &[
                ("1_1_0.LS0", before[2]),
                ("1_1_0.LS0", before[0]),
                ("2_2.LS0", before[2]),
            ],
            &[("1_1_0.ls0", before[2])],
        ]);
        let rebuilt = rebuild_ls0(
            &original,
            "ENGLISH/1_1_0.LS0",
            &edit("", offsets[1], 7000).replacements,
            &AtomicBool::new(false),
        )
        .unwrap();
        let after = &rebuilt.after;
        let mut patched = map.clone();
        let banks = [("1_1_0.LS0", &before[..], &after[..])];
        let changed = patch_map(&mut patched, "ENGLISH/MAPS.LM0", &banks).unwrap();
        // Not the resource at the first stream, which didn't move.
        assert_eq!(changed, 2);

        let found = resources_in(&patched, "", &[("1_1_0.LS0", after)]).unwrap();
        let indices: Vec<_> = found.iter().map(|&(_, _, index)| index).collect();
        assert_eq!(indices, [2, 0, 2]);
        // The other bank's resource and the container are as they were.
        assert_eq!(
            resources_in(&patched, "", &[("2_2.LS0", &before)])
                .unwrap()
                .len(),
            1
        );
        let changed: Vec<_> = (0..map.len())
            .filter(|&index| map[index] != patched[index])
            .collect();
        let resources: Vec<_> = found.iter().map(|&(resource, ..)| resource).collect();
        assert!(changed.iter().all(|&byte| {
            resources
                .iter()
                .any(|&resource| (resource..resource + RESOURCE_SIZE).contains(&byte))
        }));
        assert!(!changed.is_empty());
    }

    #[test]
    fn a_map_that_points_elsewhere_in_the_bank_is_refused() {
        let (original, _) = bank(&[3000, 5000]);
        let before = placements(&original);
        let unknown = Placement {
            offset: before[1].offset + 4,
            ..before[1]
        };
        let shorter = Placement {
            length: before[1].length - 1,
            ..before[1]
        };
        for placement in [unknown, shorter] {
            let map = map_of(&[&[("1_1_0.LS0", before[0])], &[("1_1_0.LS0", placement)]]);
            let banks = [("1_1_0.LS0", &before[..], &before[..])];
            let error = patch_map(&mut map.clone(), "ENGLISH/MAPS.LM0", &banks)
                .unwrap_err()
                .to_string();
            assert!(error.contains("isn't one of the streams"), "{error}");
        }
    }

    #[test]
    fn saving_another_length_changes_the_map_too() {
        let (game, original, offsets, map) = game_with_map();
        let cancelled = AtomicBool::new(false);
        let english = game.path().join("Sounds/ENGLISH");
        let edits = [
            edit("ENGLISH/1_1_0.LS0", offsets[1], 7000),
            edit("ENGLISH/1_1_1.LS0", offsets[0], 1500),
        ];
        let report = save(game.path(), &edits, &SaveTarget::InPlace, &cancelled).unwrap();
        assert!(report.is_complete(), "{:?}", report.failures);
        assert_eq!(report.saved, ["ENGLISH/1_1_0.LS0", "ENGLISH/1_1_1.LS0"]);
        assert_eq!(report.maps, ["ENGLISH/MAPS.LM0"]);
        assert_eq!(report.backups.len(), 3);
        assert_eq!(fs::read(english.join("MAPS.LM0.orig")).unwrap(), map);

        // Every resource is at its stream in the saved bank.
        let saved_map = fs::read(english.join("MAPS.LM0")).unwrap();
        for (name, count) in [("1_1_0.LS0", 5), ("1_1_1.LS0", 3)] {
            let bank = fs::read(english.join(name)).unwrap();
            assert_ne!(bank, original);
            let streams = placements(&bank);
            let found = resources_in(&saved_map, "", &[(name, &streams)]).unwrap();
            assert_eq!(found.len(), count, "{name}");
        }

        // A replacement of the same length leaves the map alone.
        let bank = fs::read(english.join("1_1_0.LS0")).unwrap();
        let last = placements(&bank)[2];
        let edits = [edit("ENGLISH/1_1_0.LS0", last.offset, 1000)];
        let report = save(game.path(), &edits, &SaveTarget::InPlace, &cancelled).unwrap();
        assert_eq!(report.saved, ["ENGLISH/1_1_0.LS0"]);
        assert!(report.maps.is_empty());
        assert_eq!(fs::read(english.join("MAPS.LM0")).unwrap(), saved_map);
    }

    #[test]
    fn sounds_stored_in_a_map_are_spliced_in_at_their_length() {
        use crate::sound_map_file::tests::map_with_clips;

        let map = map_with_clips(
            &[&[(1, "DOOR.wav", &[1, 2, 3]), (2, "STEP.wav", &[4, 5])]],
            &[],
        );
        let file = SoundMapFile::parse(&map).unwrap();
        let clips = file.stored_clips(&file.areas()[0]);
        let step = clips[1].offset;
        let replacement = |offset, samples: &[i16]| Replacement {
            offset,
            stream: samples
                .iter()
                .flat_map(|sample| sample.to_le_bytes())
                .collect(),
        };

        let spliced =
            splice_clips(map.clone(), "MAPS.SM0", &[replacement(step, &[-1, -2])]).unwrap();
        let start = step as usize;
        assert_eq!(spliced[..start], map[..start]);
        assert_eq!(spliced[start..start + 4], [0xFF, 0xFF, 0xFE, 0xFF]);
        assert_eq!(spliced[start + 4..], map[start + 4..]);

        for (replacements, error) in [
            (
                vec![replacement(step, &[1, 2, 3])],
                "has to keep its length",
            ),
            (
                vec![replacement(step + 2, &[1])],
                "no sound stored at offset",
            ),
            (
                vec![replacement(step, &[1, 2]), replacement(step, &[3, 4])],
                "more than once",
            ),
        ] {
            let found = splice_clips(map.clone(), "MAPS.SM0", &replacements)
                .unwrap_err()
                .to_string();
            assert!(found.contains(error), "{found}");
        }
    }

    #[test]
    fn a_map_with_sounds_of_its_own_also_follows_its_banks() {
        use crate::sound_map_file::tests::map_with_clips;

        let (game, bank, offsets) = game();
        let streams: Vec<_> = placements(&bank)
            .into_iter()
            .map(|stream| ("1_1_0.LS0", stream))
            .collect();
        let map = map_with_clips(&[&[(1, "DOOR.wav", &[1, 2, 3])]], &streams);
        let english = game.path().join("Sounds/ENGLISH");
        fs::write(english.join("MAPS.LM0"), &map).unwrap();
        let file = SoundMapFile::parse(&map).unwrap();
        let door = file.stored_clips(&file.areas()[0])[0].offset;
        let edits = [
            BankEdit {
                path: "ENGLISH/MAPS.LM0".to_owned(),
                replacements: vec![Replacement {
                    offset: door,
                    stream: vec![9; 6],
                }],
            },
            edit("ENGLISH/1_1_0.LS0", offsets[1], 7000),
        ];
        let report = save(
            game.path(),
            &edits,
            &SaveTarget::InPlace,
            &AtomicBool::new(false),
        )
        .unwrap();
        assert!(report.is_complete(), "{:?}", report.failures);
        assert_eq!(report.saved, ["ENGLISH/MAPS.LM0", "ENGLISH/1_1_0.LS0"]);
        assert!(report.maps.is_empty());
        assert_eq!(fs::read(english.join("MAPS.LM0.orig")).unwrap(), map);

        // The map has both its own replacement and its bank's new places.
        let saved = fs::read(english.join("MAPS.LM0")).unwrap();
        let start = door as usize;
        assert_eq!(saved[start..start + 6], [9; 6]);
        let bank = placements(&fs::read(english.join("1_1_0.LS0")).unwrap());
        let found = resources_in(&saved, "", &[("1_1_0.LS0", &bank)]).unwrap();
        assert_eq!(found.len(), 3);
    }

    #[test]
    fn length_changes_are_checked_before_saving() {
        let (game, _, offsets, _) = game_with_map();
        let cancelled = AtomicBool::new(false);
        let check = |path, offset| check_length_change(game.path(), path, offset, &cancelled);
        assert!(check("ENGLISH/1_1_0.LS0", offsets[1]).is_ok());
        assert!(check("ENGLISH/1_1_0.LS0", offsets[1] + 4).is_err());

        // A bank the map points at somewhere else.
        let english = game.path().join("Sounds/ENGLISH");
        let mut bank = fs::read(english.join("1_1_1.LS0")).unwrap();
        bank.truncate(offsets[2] as usize);
        fs::write(english.join("1_1_1.LS0"), bank).unwrap();
        let error = check("ENGLISH/1_1_1.LS0", offsets[0]).unwrap_err();
        assert!(
            error.to_string().contains("isn't one of the streams"),
            "{error}"
        );

        fs::remove_file(english.join("MAPS.LM0")).unwrap();
        let error = check("ENGLISH/1_1_0.LS0", offsets[1]).unwrap_err();
        assert!(error.to_string().contains("MAPS.LM0"), "{error}");
    }

    #[test]
    fn another_length_needs_the_map() {
        let (game, original, offsets) = game();
        let edits = [edit("ENGLISH/1_1_0.LS0", offsets[0], 2000)];
        let error = save(
            game.path(),
            &edits,
            &SaveTarget::InPlace,
            &AtomicBool::new(false),
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("MAPS.LM0"), "{error}");
        let path = game.path().join("Sounds/ENGLISH/1_1_0.LS0");
        assert_eq!(fs::read(path).unwrap(), original);
    }

    #[test]
    fn a_map_and_its_banks_are_saved_together_or_not_at_all() {
        let (game, _, offsets, _) = game_with_map();
        let output = tempfile::tempdir().unwrap();
        let exported = output.path().join("Sounds/ENGLISH");
        fs::create_dir_all(&exported).unwrap();
        // An earlier export's map, and a folder where the second bank goes.
        fs::write(exported.join("MAPS.LM0"), b"an earlier map").unwrap();
        fs::create_dir(exported.join("1_1_1.LS0")).unwrap();
        let edits = [
            edit("ENGLISH/1_1_0.LS0", offsets[1], 7000),
            edit("ENGLISH/1_1_1.LS0", offsets[0], 1500),
        ];
        let report = save(
            game.path(),
            &edits,
            &SaveTarget::Folder(output.path().to_owned()),
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(report.failures.len(), 1);
        let failure = &report.failures[0];
        assert!(failure.contains("1_1_1.LS0"), "{failure}");
        assert!(
            failure.contains("ENGLISH/MAPS.LM0, ENGLISH/1_1_0.LS0 weren't saved either"),
            "{failure}"
        );
        assert!(report.written.is_empty());
        assert!(report.saved.is_empty() && report.maps.is_empty());
        // What was written before the failure is put back.
        assert_eq!(
            fs::read(exported.join("MAPS.LM0")).unwrap(),
            b"an earlier map"
        );
        assert!(!exported.join("1_1_0.LS0").exists());
    }

    /// Exports every English bank with its first and last streams replaced,
    /// and checks that nothing else in it changed.
    #[test]
    #[ignore = "requires local Splinter Cell game files; set SC1_GAME_DIR"]
    fn saves_every_english_bank() {
        let game_root = std::env::var_os("SC1_GAME_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../../Splinter Cell"));
        let cancelled = AtomicBool::new(false);
        let banks = crate::discover_banks_with_progress(
            &game_root,
            "ENGLISH",
            &mut crate::GameCache::default(),
            &cancelled,
            |_, _, _| {},
        )
        .unwrap();
        let mut edits = Vec::new();
        for bank in banks.iter().filter(|bank| bank.sound_kind.is_none()) {
            let (Some(first), Some(last)) = (bank.segments.first(), bank.segments.last()) else {
                continue;
            };
            let mut replacements = Vec::new();
            for segment in [first, last] {
                if replacements.len() == 1 && first.id == last.id {
                    break;
                }
                let samples = tone(segment.sample_count as usize, 6000.0);
                let stream =
                    encode_replacement(&game_root, &bank.path, segment, &samples, &cancelled)
                        .unwrap();
                replacements.push(Replacement {
                    offset: segment.stream_range().unwrap().0,
                    stream,
                });
            }
            edits.push(BankEdit {
                path: bank.path.clone(),
                replacements,
            });
        }
        assert!(edits.len() > 30, "only {} banks", edits.len());

        let output = tempfile::tempdir().unwrap();
        let target = SaveTarget::Folder(output.path().to_owned());
        let report = save(&game_root, &edits, &target, &cancelled).unwrap();
        assert!(report.is_complete(), "{:?}", report.failures);
        assert_eq!(report.written.len(), edits.len());

        let sounds = find_sounds_directory(&game_root).unwrap();
        for (edit, written) in edits.iter().zip(&report.written) {
            let original = fs::read(sounds.join(&edit.path)).unwrap();
            let saved = fs::read(written).unwrap();
            assert_eq!(saved.len(), original.len(), "{}", edit.path);
            let mut expected = original.clone();
            for replacement in &edit.replacements {
                let start = replacement.offset as usize;
                expected[start..start + replacement.stream.len()]
                    .copy_from_slice(&replacement.stream);
            }
            assert!(saved == expected, "{}: other bytes changed", edit.path);
        }
    }

    /// Exports every English bank with its first stream made longer and its
    /// last shorter, and checks that every other stream moved intact, and
    /// that the map points at each stream where it is now.
    #[test]
    #[ignore = "requires local Splinter Cell game files; set SC1_GAME_DIR"]
    fn moves_streams_in_every_english_bank() {
        let game_root = std::env::var_os("SC1_GAME_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../../Splinter Cell"));
        let cancelled = AtomicBool::new(false);
        let banks = crate::discover_banks(&game_root, "ENGLISH").unwrap();
        let mut edits = Vec::new();
        for bank in banks.iter().filter(|bank| bank.sound_kind.is_none()) {
            let (Some(first), Some(last)) = (bank.segments.first(), bank.segments.last()) else {
                continue;
            };
            let mut lengths = vec![(first, first.sample_count as usize + 1000)];
            if last.id != first.id {
                lengths.push((last, last.sample_count as usize / 2));
            }
            let replacements = lengths
                .into_iter()
                .map(|(segment, length)| Replacement {
                    offset: segment.stream_range().unwrap().0,
                    stream: encode_replacement(
                        &game_root,
                        &bank.path,
                        segment,
                        &tone(length, 6000.0),
                        &cancelled,
                    )
                    .unwrap(),
                })
                .collect();
            edits.push(BankEdit {
                path: bank.path.clone(),
                replacements,
            });
        }
        assert!(edits.len() > 30, "only {} banks", edits.len());

        let output = tempfile::tempdir().unwrap();
        let target = SaveTarget::Folder(output.path().to_owned());
        let report = save(&game_root, &edits, &target, &cancelled).unwrap();
        assert!(report.is_complete(), "{:?}", report.failures);
        assert_eq!(report.saved.len(), edits.len());
        // The language's own map, and each extra map's.
        assert_eq!(report.maps.len(), 4, "{:?}", report.maps);

        let sounds = find_sounds_directory(&game_root).unwrap();
        let exported = output.path().canonicalize().unwrap().join("Sounds");
        let mut patched_resources = std::collections::HashMap::<&str, Vec<usize>>::new();
        for edit in &edits {
            let (folder, bank_file) = edit.path.rsplit_once('/').unwrap();
            let map_path = report
                .maps
                .iter()
                .find(|map| map.rsplit_once('/').unwrap().0 == folder)
                .unwrap();
            let original_map = fs::read(sounds.join(map_path)).unwrap();
            let saved_map = fs::read(exported.join(map_path)).unwrap();
            let original = fs::read(sounds.join(&edit.path)).unwrap();
            let saved = fs::read(exported.join(&edit.path)).unwrap();
            let (before, after) = (placements(&original), placements(&saved));
            assert_eq!(before.len(), after.len(), "{}", edit.path);
            for (index, (old, new)) in before.iter().zip(&after).enumerate() {
                let replaced = index == 0 || index == before.len() - 1;
                if !replaced {
                    assert_eq!(
                        (old.length, old.sample_count),
                        (new.length, new.sample_count)
                    );
                    let stream = |bytes: &[u8], at: &Placement| {
                        bytes[at.offset as usize..(at.offset + at.length) as usize].to_vec()
                    };
                    assert!(
                        stream(&original, old) == stream(&saved, new),
                        "{}",
                        edit.path
                    );
                }
            }
            let old = resources_in(&original_map, "", &[(bank_file, &before)]).unwrap();
            let new = resources_in(&saved_map, "", &[(bank_file, &after)]).unwrap();
            assert_eq!(old, new, "{}: resources moved to other streams", edit.path);
            assert!(!new.is_empty(), "{}", edit.path);
            patched_resources
                .entry(map_path)
                .or_default()
                .extend(new.into_iter().map(|(resource, ..)| resource));
        }
        // Nothing but those resources' placements changed in the maps.
        let fields = [SIZE, OFFSET, SAMPLE_COUNT, SIZE_AGAIN];
        for (map_path, resources) in patched_resources {
            let original_map = fs::read(sounds.join(map_path)).unwrap();
            let saved_map = fs::read(exported.join(map_path)).unwrap();
            assert_eq!(saved_map.len(), original_map.len());
            for (index, (old, new)) in original_map.iter().zip(&saved_map).enumerate() {
                if old != new {
                    assert!(
                        resources
                            .iter()
                            .any(|&resource| fields.iter().any(|&field| {
                                (resource + field..resource + field + 4).contains(&index)
                            })),
                        "byte {index:#x} of {map_path} changed"
                    );
                }
            }
        }
    }

    /// Replaces the first stream in every language's `Music_Common.LS0`, and
    /// each extra map's, with a longer one, through the map sound that plays
    /// it, and checks that the bank and the map beside it follow.
    #[test]
    #[ignore = "requires local Splinter Cell game files; set SC1_GAME_DIR"]
    fn replaces_streams_in_music_common() {
        let game_root = std::env::var_os("SC1_GAME_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../../Splinter Cell"));
        let cancelled = AtomicBool::new(false);
        let sounds = find_sounds_directory(&game_root).unwrap();
        let mut edits = Vec::new();
        let languages = crate::discover_languages(&game_root).unwrap();
        assert!(languages.len() > 1);
        for language in &languages {
            let banks = crate::discover_banks(&game_root, &language.directory).unwrap();
            let before = edits.len();
            for bank in &banks {
                let streams: Vec<_> = bank
                    .segments
                    .iter()
                    .filter_map(|segment| Some((segment, segment.replaceable_stream(&bank.path)?)))
                    .filter(|(_, stream)| stream.format == StreamFormat::UbisoftAdpcm)
                    .collect();
                if bank.sound_kind.is_none() || streams.is_empty() {
                    continue;
                }
                // The scan finds the streams the map points at, and no others.
                let file = streams[0].1.path;
                let mut found: Vec<_> = streams
                    .iter()
                    .map(|(segment, stream)| {
                        assert_eq!(stream.path, file, "{}", segment.id);
                        assert_eq!(stream.offsets.len(), 1, "{}", segment.id);
                        Placement {
                            offset: stream.offsets[0],
                            length: stream.byte_length,
                            sample_count: segment.sample_count,
                        }
                    })
                    .collect();
                // Each is a stream of its own.
                found.sort_by_key(|placement| placement.offset);
                let count = found.len();
                found.dedup_by_key(|placement| placement.offset);
                assert_eq!(found.len(), count, "{file}");
                assert_eq!(
                    found,
                    placements(&fs::read(sounds.join(file)).unwrap()),
                    "{file}"
                );

                let (segment, _) = streams
                    .iter()
                    .find(|(_, stream)| stream.offsets == [0])
                    .unwrap();
                let samples = tone(segment.sample_count as usize + 5000, 6000.0);
                let stream =
                    encode_replacement(&game_root, &bank.path, segment, &samples, &cancelled)
                        .unwrap();
                check_length_change(&game_root, file, 0, &cancelled).unwrap();
                edits.push(BankEdit {
                    path: file.to_owned(),
                    replacements: vec![Replacement { offset: 0, stream }],
                });
            }
            // At least the language's own.
            assert!(edits.len() > before, "{}", language.directory);
        }

        let output = tempfile::tempdir().unwrap();
        let target = SaveTarget::Folder(output.path().to_owned());
        let report = save(&game_root, &edits, &target, &cancelled).unwrap();
        assert!(report.is_complete(), "{:?}", report.failures);
        assert_eq!(report.maps.len(), edits.len(), "{:?}", report.maps);
        let exported = output.path().canonicalize().unwrap().join("Sounds");
        for edit in &edits {
            let (folder, file) = edit.path.rsplit_once('/').unwrap();
            let map = fs::read(exported.join(folder).join("MAPS.LM0")).unwrap();
            let saved = placements(&fs::read(exported.join(&edit.path)).unwrap());
            assert!(saved[0].sample_count > 0);
            let found = resources_in(&map, "", &[(file, &saved)]).unwrap();
            assert!(
                found.iter().any(|&(_, _, stream)| stream == 0),
                "{}",
                edit.path
            );
        }
    }

    /// Checks that every sound stored in the English maps as mono 16-bit PCM
    /// is where the app says, by comparing each copy with what vgmstream
    /// plays, then replaces one in `MAPS.SM0` and one in an extra map's
    /// `MAPS.LM0`, and checks that every copy changed and nothing else.
    #[test]
    #[ignore = "requires local Splinter Cell game files; set SC1_GAME_DIR"]
    fn replaces_sounds_stored_in_maps() {
        let game_root = std::env::var_os("SC1_GAME_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../../Splinter Cell"));
        let cancelled = AtomicBool::new(false);
        let sounds = find_sounds_directory(&game_root).unwrap();
        let banks = crate::discover_banks(&game_root, "ENGLISH").unwrap();
        let mut maps = std::collections::BTreeMap::<&str, Vec<u8>>::new();
        let mut chosen = std::collections::BTreeMap::<&str, (&Segment, Vec<u64>)>::new();
        let mut checked = 0;
        for bank in &banks {
            for segment in &bank.segments {
                let Some(stream) = segment.replaceable_stream(&bank.path) else {
                    continue;
                };
                if stream.format != StreamFormat::Pcm16 {
                    continue;
                }
                assert_eq!(stream.path, bank.path);
                let map = maps
                    .entry(&bank.path)
                    .or_insert_with(|| fs::read(sounds.join(&bank.path)).unwrap());
                let length = stream.byte_length as usize;
                let first = &map[stream.offsets[0] as usize..][..length];
                for &offset in stream.offsets {
                    assert!(&map[offset as usize..][..length] == first, "{}", segment.id);
                }
                let played =
                    crate::decode_segment(&game_root, &bank.path, segment, &cancelled).unwrap();
                let stored: Vec<i16> = first
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|pair| i16::from_le_bytes(*pair))
                    .collect();
                assert!(played.samples == stored, "{}", segment.id);
                checked += 1;
                chosen
                    .entry(&bank.path)
                    .or_insert((segment, stream.offsets.to_vec()));
            }
        }
        assert!(checked > 900, "only {checked} sounds");

        let edits: Vec<_> = ["MAPS.SM0", "1_6_1_1kolacell/English/MAPS.LM0"]
            .into_iter()
            .map(|path| {
                let (segment, offsets) = &chosen[path];
                let samples = tone(segment.sample_count as usize, 6000.0);
                let stream =
                    encode_replacement(&game_root, path, segment, &samples, &cancelled).unwrap();
                BankEdit {
                    path: path.to_owned(),
                    replacements: offsets
                        .iter()
                        .map(|&offset| Replacement {
                            offset,
                            stream: stream.clone(),
                        })
                        .collect(),
                }
            })
            .collect();
        let output = tempfile::tempdir().unwrap();
        let target = SaveTarget::Folder(output.path().to_owned());
        let report = save(&game_root, &edits, &target, &cancelled).unwrap();
        assert!(report.is_complete(), "{:?}", report.failures);
        assert_eq!(
            report.saved,
            ["MAPS.SM0", "1_6_1_1kolacell/English/MAPS.LM0"]
        );
        assert!(report.maps.is_empty());
        let exported = output.path().canonicalize().unwrap().join("Sounds");
        for edit in &edits {
            let original = &maps[edit.path.as_str()];
            let saved = fs::read(exported.join(&edit.path)).unwrap();
            let mut expected = original.clone();
            for replacement in &edit.replacements {
                let start = replacement.offset as usize;
                expected[start..start + replacement.stream.len()]
                    .copy_from_slice(&replacement.stream);
            }
            assert_ne!(&expected, original);
            assert!(saved == expected, "{}: other bytes changed", edit.path);
        }
    }
}
