//! The sounds in the game's sound maps, which vgmstream reads.
//!
//! `MAPS.SM0` holds the sound effects and music every language shares, and
//! each language's `MAPS.LM0` the localized ones. A map is split into areas,
//! one for each level plus `fix` and `menu`, and lists every sound an area
//! plays. Some sounds' audio is in the map itself; others point at a stream
//! file such as `Music_Common.SS0`, `STREAM.SS0` or a level's `.LS0` bank.
//! The extra maps have their own `MAPS.SM0` and `MAPS.LM0`.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::vgmstream::{SoundInfo, Vgmstream};
use crate::{Bank, Error, Segment, SegmentSource};

/// The map files, relative to `Sounds`, that have the sounds for a language
/// folder, such as `MAPS.SM0` and `ENGLISH/MAPS.LM0`.
pub(crate) fn map_files(sounds: &Path, map: Option<&str>, language_folder: &str) -> Vec<String> {
    let shared_folder = map.unwrap_or("");
    [(shared_folder, "MAPS.SM0"), (language_folder, "MAPS.LM0")]
        .into_iter()
        .filter_map(|(folder, file_name)| {
            let path = crate::find_file(&sounds.join(folder), file_name).ok()?;
            let name = path.file_name()?.to_str()?;
            Some(if folder.is_empty() {
                name.to_owned()
            } else {
                format!("{folder}/{name}")
            })
        })
        .collect()
}

/// What a bank of map sounds holds, in the order banks are listed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum SoundKind {
    /// Short sounds stored in `MAPS.SM0`.
    Effects,
    /// Short localized sounds stored in `MAPS.LM0`, mostly characters' barks.
    Voices,
    /// Music from `Music_*.SS0`, and the sequences that string it together.
    Music,
    /// Music from each language's `Music_*.LS0`.
    LocalizedMusic,
    /// Long streams from `STREAM.SS0` and other stream files, some of them
    /// in several stereo layers.
    Streams,
}

impl SoundKind {
    fn label(self) -> &'static str {
        match self {
            SoundKind::Effects => "sound effects",
            SoundKind::Voices => "voices",
            SoundKind::Music => "music",
            SoundKind::LocalizedMusic => "localized music",
            SoundKind::Streams => "streams",
        }
    }
}

/// Lists the sounds in the map file at `path`, relative to `sounds`, as banks
/// of one area's sounds of one kind. Sounds in numeric `.LS0` banks are left
/// out, since those banks are scanned on their own. A bank of sounds that
/// can't be read has no kind.
pub(crate) fn read_map_banks(
    sounds: &Path,
    path: &str,
    map: Option<&str>,
    cancelled: &AtomicBool,
) -> Result<Vec<(Option<SoundKind>, Bank)>, Error> {
    let is_localized = path
        .rsplit_once('.')
        .is_some_and(|(_, extension)| extension.eq_ignore_ascii_case("LM0"));
    let name = path.rsplit('/').next().unwrap_or(path).to_owned();
    let size = std::fs::metadata(sounds.join(path)).map_or(0, |metadata| metadata.len());
    let unreadable = |issue: String| Bank {
        name: name.clone(),
        map: map.map(str::to_owned),
        area: None,
        path: path.to_owned(),
        code: String::new(),
        mission_name: None,
        size,
        issue: Some(issue),
        segments: Vec::new(),
    };

    let first = match Vgmstream::new()?.open(sounds, path, 1) {
        Ok(first) => first,
        Err(error) => return Ok(vec![(None, unreadable(error))]),
    };
    let sound_count = first.subsong_count.max(1);
    let mut banks = BTreeMap::<(String, SoundKind), Vec<Segment>>::new();
    let mut failures = 0;
    for (subsong, info) in open_sounds(sounds, path, sound_count, cancelled)? {
        let Some(info) = info else {
            failures += 1;
            continue;
        };
        let entry = MapEntry::parse(&info.stream_name);
        let Some(kind) = entry.kind(is_localized, &info) else {
            continue;
        };
        let segments = banks.entry((entry.area.to_owned(), kind)).or_default();
        segments.push(Segment {
            id: format!("{path}#{subsong}"),
            source: SegmentSource::MapSound {
                subsong,
                key: entry.key,
            },
            name: Some(entry.display_name()),
            sample_count: info.sample_count,
            sample_rate: info.sample_rate,
            channels: info.channels,
            codec: info.codec,
            segment: segments.len(),
        });
    }

    let mut banks: Vec<_> = banks
        .into_iter()
        .map(|((area, kind), segments)| {
            let code = crate::leading_mission_code(&area).unwrap_or_default();
            let prefix = if code.is_empty() { &area } else { &code };
            let bank = Bank {
                name: format!("{prefix} {}", kind.label()),
                map: map.map(str::to_owned),
                area: Some(area.clone()),
                path: path.to_owned(),
                code,
                mission_name: None,
                size,
                issue: None,
                segments,
            };
            (Some(kind), bank)
        })
        .collect();
    if failures > 0 {
        banks.push((
            None,
            unreadable(format!(
                "vgmstream could not open {failures} of the {sound_count} sounds in this map."
            )),
        ));
    }
    Ok(banks)
}

/// Opens sounds 1 to `count` of the map at `path` to read their details,
/// returning them in order, with `None` for those vgmstream can't open.
///
/// vgmstream reads the map's index again to open each sound, so this is spread
/// over several threads, each with its own vgmstream. Opening a sound takes
/// longer the later it is in the map, so threads take turns.
fn open_sounds(
    sounds: &Path,
    path: &str,
    count: u32,
    cancelled: &AtomicBool,
) -> Result<Vec<(u32, Option<SoundInfo>)>, Error> {
    let threads =
        std::thread::available_parallelism().map_or(1, |threads| threads.get().min(8)) as u32;
    let results = std::thread::scope(|scope| {
        let workers: Vec<_> = (1..=threads.min(count))
            .map(|first| {
                scope.spawn(move || -> Result<Vec<_>, Error> {
                    let mut vgmstream = Vgmstream::new()?;
                    let mut sounds_opened = Vec::new();
                    for subsong in (first..=count).step_by(threads as usize) {
                        if cancelled.load(Ordering::Relaxed) {
                            return Err(Error::Cancelled);
                        }
                        sounds_opened.push((subsong, vgmstream.open(sounds, path, subsong).ok()));
                    }
                    Ok(sounds_opened)
                })
            })
            .collect();
        workers
            .into_iter()
            .map(|worker| worker.join().expect("a sound map worker panicked"))
            .collect::<Result<Vec<_>, _>>()
    })?;
    let mut results: Vec<_> = results.into_iter().flatten().collect();
    results.sort_by_key(|(subsong, _)| *subsong);
    Ok(results)
}

/// Where vgmstream says a map's sound is, from a stream name such as
/// `1_1_0Tbilisi/0012/01-00130086/TAWCHI02.wav`: the area, the entry's type
/// and sound key, and the sound's name or the file holding its audio.
#[derive(Debug, PartialEq, Eq)]
struct MapEntry<'a> {
    area: &'a str,
    /// 0x01 for a sound, 0x0C for a sequence of sounds, and 0x0D for a sound
    /// in layers.
    entry_type: Option<u32>,
    key: Option<u32>,
    name: &'a str,
}

const SEQUENCE: u32 = 0x0C;
const LAYERS: u32 = 0x0D;

impl<'a> MapEntry<'a> {
    fn parse(stream_name: &'a str) -> Self {
        let parts: Vec<&str> = stream_name.split('/').collect();
        let (entry_type, key) = parts.get(2).and_then(|field| field.split_once('-')).map_or(
            (None, None),
            |(entry_type, key)| {
                (
                    u32::from_str_radix(entry_type, 16).ok(),
                    u32::from_str_radix(key, 16).ok(),
                )
            },
        );
        MapEntry {
            area: if parts.len() == 4 { parts[0] } else { "" },
            entry_type,
            key,
            name: parts.last().copied().unwrap_or_default(),
        }
    }

    /// The stream file holding the sound's audio, such as `STREAM.SS0`, or
    /// `None` when the audio is in the map.
    fn stream_file(&self) -> Option<&'a str> {
        let (_, extension) = self.name.rsplit_once('.')?;
        ["SS0", "LS0"]
            .iter()
            .any(|stream| extension.eq_ignore_ascii_case(stream))
            .then_some(self.name)
    }

    /// The kind of bank the sound belongs in, or `None` for sounds in a
    /// numeric `.LS0` bank.
    fn kind(&self, is_localized: bool, info: &SoundInfo) -> Option<SoundKind> {
        if let Some(file) = self.stream_file() {
            return if crate::mission_code(file).is_some() {
                None
            } else if file
                .get(..6)
                .is_some_and(|prefix| prefix.eq_ignore_ascii_case("Music_"))
            {
                Some(if is_localized {
                    SoundKind::LocalizedMusic
                } else {
                    SoundKind::Music
                })
            } else {
                Some(SoundKind::Streams)
            };
        }
        Some(match self.entry_type {
            _ if info.channels > 2 => SoundKind::Streams,
            Some(LAYERS) => SoundKind::Streams,
            Some(SEQUENCE) => SoundKind::Music,
            _ if is_localized => SoundKind::Voices,
            _ => SoundKind::Effects,
        })
    }

    /// The sound's name without its extension, such as `TAWCHI02`, the stream
    /// file holding it, or what kind of entry it is.
    fn display_name(&self) -> String {
        if let Some(file) = self.stream_file() {
            return file.to_owned();
        }
        match self.entry_type {
            Some(SEQUENCE) => format!("Sequence ({})", self.name),
            Some(LAYERS) => "Layers".to_owned(),
            _ => self
                .name
                .rsplit_once('.')
                .map_or(self.name, |(stem, _)| stem)
                .to_owned(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(channels: u32) -> SoundInfo {
        SoundInfo {
            subsong_count: 1,
            channels,
            sample_rate: 22050,
            sample_count: 1,
            codec: String::new(),
            stream_name: String::new(),
        }
    }

    #[test]
    fn parses_vgmstream_stream_names() {
        assert_eq!(
            MapEntry::parse("1_1_0Tbilisi/0012/01-00130086/TAWCHI02.wav"),
            MapEntry {
                area: "1_1_0Tbilisi",
                entry_type: Some(0x01),
                key: Some(0x0013_0086),
                name: "TAWCHI02.wav",
            }
        );
        let unexpected = MapEntry::parse("TAWCHI02.wav");
        assert_eq!(unexpected.area, "");
        assert_eq!(unexpected.key, None);
        assert_eq!(unexpected.display_name(), "TAWCHI02");
    }

    #[test]
    fn sorts_map_sounds_into_kinds() {
        let kind = |stream_name, is_localized, channels| {
            MapEntry::parse(stream_name).kind(is_localized, &info(channels))
        };
        assert_eq!(
            kind("fix/0001/01-00050001/ROGEAR_2.wav", false, 1),
            Some(SoundKind::Effects)
        );
        assert_eq!(
            kind("4_1_2ChineseEmbassy/0001/01-00130086/CSOGAZ_1.ima", true, 1),
            Some(SoundKind::Voices)
        );
        assert_eq!(
            kind("0_0_2_Training/0001/01-00050002/Music_Common.SS0", false, 2),
            Some(SoundKind::Music)
        );
        assert_eq!(
            kind("0_0_2_Training/0001/01-00660002/Music_Common.LS0", true, 1),
            Some(SoundKind::LocalizedMusic)
        );
        assert_eq!(
            kind("0_0_2_Training/0000/0c-000500f6/single", false, 2),
            Some(SoundKind::Music)
        );
        assert_eq!(
            kind("0_0_2_Training/0023/0c-001e0001/multi-loop", false, 6),
            Some(SoundKind::Streams)
        );
        assert_eq!(
            kind("0_0_2_Training/0024/0d-001e0002/STREAM.SS0", false, 6),
            Some(SoundKind::Streams)
        );
        assert_eq!(
            kind("0_0_2_Training/0001/01-00840001/0_0_2.LS0", true, 1),
            None
        );
    }

    #[test]
    fn names_map_sounds() {
        let name = |stream_name| MapEntry::parse(stream_name).display_name();
        assert_eq!(name("fix/0001/01-00050001/ROGEAR_2.wav"), "ROGEAR_2");
        assert_eq!(
            name("0_0_2_Training/0001/01-00050002/Music_Common.SS0"),
            "Music_Common.SS0"
        );
        assert_eq!(
            name("0_0_2_Training/0023/0c-001e0001/multi-loop"),
            "Sequence (multi-loop)"
        );
        assert_eq!(
            name("0_0_2_Training/0024/0d-001e0002/STREAM.SS0"),
            "STREAM.SS0"
        );
    }
}
