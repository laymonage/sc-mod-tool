//! Saving replaced segments: rebuilding `.LS0` banks with new streams, and
//! writing them over the game's files or into a folder of their own.
//!
//! Only replacements the same length as the streams they replace can be
//! saved for now, so no other stream moves and the sound maps stay as they
//! are.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use crate::banks::scan_ls0;
use crate::encoder::{STREAM_HEADER_SIZE, StreamHeader, encode_stream};
use crate::sounds_folder::{find_sounds_directory, safe_child_path};
use crate::{Error, Segment, decoder, read_segment_with_cancel};

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
    /// the saved banks.
    Folder(PathBuf),
}

/// What [`save`] wrote. A file that couldn't be written is listed in
/// `failures`, and the others are still written.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SaveReport {
    pub written: Vec<PathBuf>,
    /// The copies of the game's files made before they were first replaced.
    pub backups: Vec<PathBuf>,
    pub failures: Vec<String>,
}

impl SaveReport {
    pub fn is_complete(&self) -> bool {
        self.failures.is_empty()
    }
}

/// Encodes `samples` as a replacement for `segment`, a stream in the `.LS0`
/// bank at `bank_path`, keeping its header.
pub fn encode_replacement(
    root: &Path,
    bank_path: &str,
    segment: &Segment,
    samples: &[i16],
    cancelled: &AtomicBool,
) -> Result<Vec<u8>, Error> {
    let (offset, _) = segment
        .stream_range()
        .ok_or("Only streams in numbered .LS0 banks can be replaced.")?;
    let header = read_segment_with_cancel(
        root,
        bank_path,
        offset,
        STREAM_HEADER_SIZE as u64,
        cancelled,
    )?;
    encode_stream(&StreamHeader::parse(&header)?, samples, cancelled)
}

/// Saves `edits` to `target`. Every bank is rebuilt and checked before any
/// is written, so a problem with one leaves all the files as they were. Once
/// writing starts, it isn't cancelled.
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

    let mut banks: Vec<(&BankEdit, PathBuf, PathBuf, Vec<u8>)> = Vec::with_capacity(edits.len());
    for edit in edits.iter().filter(|edit| !edit.replacements.is_empty()) {
        // The resolved path, which has the file's own name however `edit`
        // spells it, so that two spellings of one bank are caught.
        let source = safe_child_path(&sounds, &edit.path)?;
        if banks.iter().any(|(_, other, _, _)| *other == source) {
            return Err(format!(
                "{} is edited more than once: its replacements must be saved together.",
                edit.path
            )
            .into());
        }
        let original = fs::read(&source)
            .map_err(|error| format!("Could not read {}: {error}", source.display()))?;
        let rebuilt = rebuild_ls0(&original, &edit.path, &edit.replacements, cancelled)?;
        let relative = source
            .strip_prefix(&sounds)
            .map_err(|_| format!("{} isn't in the Sounds folder.", edit.path))?
            .to_owned();
        banks.push((edit, source, relative, rebuilt));
    }
    if cancelled.load(Ordering::Relaxed) {
        return Err(Error::Cancelled);
    }

    let mut report = SaveReport::default();
    for (edit, source, relative, bytes) in banks {
        let destination = match &folder {
            Some(folder) => folder.join(relative),
            None => {
                match back_up(&source) {
                    Ok(Some(backup)) => report.backups.push(backup),
                    Ok(None) => {}
                    Err(error) => {
                        report.failures.push(format!(
                            "{} wasn't saved, as it couldn't be backed up: {error}",
                            edit.path
                        ));
                        continue;
                    }
                }
                source
            }
        };
        match write_atomically(&destination, &bytes) {
            Ok(()) => report.written.push(destination),
            Err(error) => report.failures.push(format!(
                "Could not write {}: {error}",
                destination.display()
            )),
        }
    }
    Ok(report)
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

/// `original`, the `.LS0` bank at `bank_path`, with `replacements` in place
/// of its streams.
///
/// The result is checked by scanning it again: it must have the same
/// streams, at the same offsets and of the same lengths, and the new ones
/// must decode.
pub(crate) fn rebuild_ls0(
    original: &[u8],
    bank_path: &str,
    replacements: &[Replacement],
    cancelled: &AtomicBool,
) -> Result<Vec<u8>, Error> {
    let segments = scan_ls0(original, bank_path, cancelled)?;
    let mut output = original.to_vec();
    let mut replaced = Vec::with_capacity(replacements.len());
    for replacement in replacements {
        let Some((segment, (offset, length))) = segments.iter().find_map(|segment| {
            segment
                .stream_range()
                .filter(|(offset, _)| *offset == replacement.offset)
                .map(|range| (segment, range))
        }) else {
            return Err(format!(
                "{bank_path} has no stream at offset {} to replace.",
                replacement.offset
            )
            .into());
        };
        let id = &segment.id;
        if replaced.iter().any(|&(_, other, _)| other == offset) {
            return Err(format!("{id} is replaced more than once.").into());
        }
        if replacement.stream.len() as u64 != length {
            return Err(format!(
                "The replacement for {id} is {} bytes long, but the stream it replaces is \
                 {length}. Replacements of a different length can't be saved yet.",
                replacement.stream.len()
            )
            .into());
        }
        let header = StreamHeader::parse(&replacement.stream)?;
        if header.sample_count() != segment.sample_count {
            return Err(format!(
                "The replacement for {id} has {} samples instead of {}.",
                header.sample_count(),
                segment.sample_count
            )
            .into());
        }
        let start = offset as usize;
        output[start..start + length as usize].copy_from_slice(&replacement.stream);
        replaced.push((segment, offset, length));
    }

    let layout = |segments: &[Segment]| {
        segments
            .iter()
            .map(|segment| {
                (
                    segment.stream_range(),
                    segment.sample_count,
                    segment.codec.clone(),
                )
            })
            .collect::<Vec<_>>()
    };
    if layout(&scan_ls0(&output, bank_path, cancelled)?) != layout(&segments) {
        return Err(format!(
            "Saving {bank_path} would change streams that weren't replaced, so it wasn't saved."
        )
        .into());
    }
    for (segment, offset, length) in replaced {
        let start = offset as usize;
        let stream = &output[start..start + length as usize];
        decoder::decode(stream, segment.sample_count, cancelled).map_err(|error| match error {
            Error::Cancelled => Error::Cancelled,
            Error::Failed(message) => format!(
                "The replacement for {} doesn't decode: {message}",
                segment.id
            )
            .into(),
        })?;
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SegmentSource;

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
        let start = offsets[1] as usize;
        let end = start + replacement.len();
        assert_eq!(rebuilt.len(), original.len());
        assert_eq!(rebuilt[..start], original[..start]);
        assert_eq!(rebuilt[start..end], replacement);
        assert_eq!(rebuilt[end..], original[end..]);
    }

    #[test]
    fn replacements_must_fit_a_stream() {
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
        assert!(rebuild(offsets[1], 4000).contains("different length"));
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
            edit("ENGLISH/1_1_1.LS0", offsets[0], 2000),
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
}
