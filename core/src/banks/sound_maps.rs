//! The sounds in the game's sound maps, which vgmstream reads.
//!
//! `MAPS.SM0` holds the sound effects and music every language shares, and
//! each language's `MAPS.LM0` the localized ones. A map is split into areas,
//! one for each level plus `fix` and `menu`, and lists every sound an area
//! plays. Some sounds' audio is in the map itself; others point at a stream
//! file such as `Music_Common.SS0`, `STREAM.SS0` or a level's `.LS0` bank.
//! The extra maps have their own `MAPS.SM0` and `MAPS.LM0`.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use super::missions::mission_code;
use crate::sounds_folder::find_file;
use crate::vgmstream::{SharedFile, SoundInfo, Vgmstream};
use crate::{Bank, Error, Segment, SegmentSource};

/// The map files, relative to `Sounds`, that have the sounds for a language
/// folder, such as `MAPS.SM0` and `ENGLISH/MAPS.LM0`.
pub(crate) fn map_files(sounds: &Path, map: Option<&str>, language_folder: &str) -> Vec<String> {
    let shared_folder = map.unwrap_or("");
    [(shared_folder, "MAPS.SM0"), (language_folder, "MAPS.LM0")]
        .into_iter()
        .filter_map(|(folder, file_name)| {
            let path = find_file(&sounds.join(folder), file_name).ok()?;
            let name = path.file_name()?.to_str()?;
            Some(if folder.is_empty() {
                name.to_owned()
            } else {
                format!("{folder}/{name}")
            })
        })
        .collect()
}

/// Whether the map at `path` is a language's own `MAPS.LM0`, rather than one
/// every language shares.
fn is_localized(path: &str) -> bool {
    path.rsplit_once('.')
        .is_some_and(|(_, extension)| extension.eq_ignore_ascii_case("LM0"))
}

/// What a bank of map sounds holds, in the order banks are listed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum SoundKind {
    /// Short sounds stored in `MAPS.SM0`.
    Effects,
    /// Localized voices: characters' barks stored in `MAPS.LM0`, and
    /// Lambert's alarm warnings and guards' alerts from `Music_*.LS0`.
    Voices,
    /// Music from `Music_*.SS0`, and the sequences that string it together.
    Music,
    /// Long streams from `STREAM.SS0` and other stream files, some of them
    /// in several stereo layers.
    Streams,
}

impl SoundKind {
    fn label(self) -> &'static str {
        match self {
            SoundKind::Effects => "Sound effects",
            SoundKind::Voices => "Voices",
            SoundKind::Music => "Music",
            SoundKind::Streams => "Streams",
        }
    }
}

/// Lists the sounds in the map file at `path`, relative to `sounds`, as one
/// bank for each kind, sorted by name. Sounds in numeric `.LS0` banks are left
/// out, since those banks are scanned on their own. A bank of sounds that
/// can't be read has no kind.
pub(crate) fn read_map_banks(
    sounds: &Path,
    path: &str,
    map: Option<&str>,
    cancelled: &AtomicBool,
) -> Result<Vec<Bank>, Error> {
    let is_localized = is_localized(path);
    let name = path.rsplit('/').next().unwrap_or(path).to_owned();
    let size = std::fs::metadata(sounds.join(path)).map_or(0, |metadata| metadata.len());
    let unreadable = |issue: String| Bank {
        name: name.clone(),
        map: map.map(str::to_owned),
        sound_kind: None,
        path: path.to_owned(),
        code: String::new(),
        mission_name: None,
        size,
        issue: Some(issue),
        segments: Vec::new(),
    };

    let map_file = match SharedFile::open(sounds, path) {
        Ok(map_file) => map_file,
        Err(error) => return Ok(vec![unreadable(error)]),
    };
    let first = match Vgmstream::new()?
        .sharing(Arc::clone(&map_file))
        .open(sounds, path, 1)
    {
        Ok(first) => first,
        Err(error) => return Ok(vec![unreadable(error)]),
    };
    let sound_count = first.subsong_count.max(1);
    let mut lists = SoundLists::default();
    let mut failures = 0;
    for (subsong, info) in open_sounds(sounds, path, &map_file, sound_count, cancelled)? {
        match info {
            Some(info) => lists.add(path, subsong, is_localized, info),
            None => failures += 1,
        }
    }

    let mut banks: Vec<_> = lists
        .into_sorted()
        .into_iter()
        .map(|(kind, segments)| Bank {
            name: kind.label().to_owned(),
            map: map.map(str::to_owned),
            sound_kind: Some(kind),
            path: path.to_owned(),
            code: String::new(),
            mission_name: None,
            size,
            issue: None,
            segments,
        })
        .collect();
    if failures > 0 {
        banks.push(unreadable(format!(
            "vgmstream could not open {failures} of the {sound_count} sounds in this map."
        )));
    }
    Ok(banks)
}

/// A map's sounds, in one list for each kind.
///
/// Each of the map's areas lists every sound it plays, so most sounds are
/// listed by several areas. Entries with the same key, name and length are the
/// same sound, so it's listed once, with every area that plays it.
#[derive(Default)]
struct SoundLists {
    lists: BTreeMap<SoundKind, Vec<Segment>>,
    /// Where each sound with a key is in its list.
    listed: HashMap<(SoundKind, u32, String, u32), usize>,
    /// The IDs of sounds without a name of their own, which are listed by
    /// their stream file or kind of entry instead.
    unnamed: HashSet<String>,
}

impl SoundLists {
    /// Adds sound `subsong` of the map at `path`, unless it's in a numeric
    /// `.LS0` bank.
    fn add(&mut self, path: &str, subsong: u32, is_localized: bool, info: SoundInfo) {
        let entry = MapEntry::parse(&info.stream_name);
        let Some(kind) = entry.kind(is_localized, &info) else {
            return;
        };
        let name = entry.display_name();
        let segments = self.lists.entry(kind).or_default();
        let identity = entry
            .key
            .map(|key| (kind, key, name.clone(), info.sample_count));
        if let Some(index) = identity
            .as_ref()
            .and_then(|identity| self.listed.get(identity))
        {
            if let SegmentSource::MapSound { areas, .. } = &mut segments[*index].source
                && !entry.area.is_empty()
                && !areas.iter().any(|area| area == entry.area)
            {
                areas.push(entry.area.to_owned());
            }
            return;
        }
        if let Some(identity) = identity {
            self.listed.insert(identity, segments.len());
        }
        let id = format!("{path}#{subsong}");
        if !entry.has_name() {
            self.unnamed.insert(id.clone());
        }
        segments.push(Segment {
            id,
            source: SegmentSource::MapSound {
                subsong,
                key: entry.key,
                areas: [entry.area]
                    .into_iter()
                    .filter(|area| !area.is_empty())
                    .map(str::to_owned)
                    .collect(),
            },
            name: Some(name),
            sample_count: info.sample_count,
            sample_rate: info.sample_rate,
            channels: info.channels,
            codec: info.codec,
            segment: 0,
        });
    }

    /// The lists, each sorted by name, with sounds without a name of their
    /// own last, and numbered.
    fn into_sorted(self) -> BTreeMap<SoundKind, Vec<Segment>> {
        let mut lists = self.lists;
        for segments in lists.values_mut() {
            segments.sort_by_cached_key(|segment| {
                let (key, subsong) = match segment.source {
                    SegmentSource::MapSound { subsong, key, .. } => (key, subsong),
                    SegmentSource::Stream { .. } => (None, 0),
                };
                (
                    self.unnamed.contains(&segment.id),
                    segment.name.as_deref().map(str::to_lowercase),
                    key,
                    subsong,
                )
            });
            for (index, segment) in segments.iter_mut().enumerate() {
                segment.segment = index;
            }
        }
        lists
    }
}

/// Opens sounds 1 to `count` of the map at `path` to read their details,
/// returning them in order, with `None` for those vgmstream can't open.
///
/// vgmstream reads the map's index again to open each sound, so this is spread
/// over several threads, each with its own vgmstream, which all read the map
/// through `map_file`. Opening a sound takes longer the later it is in the
/// map, so threads take turns.
fn open_sounds(
    sounds: &Path,
    path: &str,
    map_file: &Arc<SharedFile>,
    count: u32,
    cancelled: &AtomicBool,
) -> Result<Vec<(u32, Option<SoundInfo>)>, Error> {
    let threads =
        std::thread::available_parallelism().map_or(1, |threads| threads.get().min(8)) as u32;
    let results = std::thread::scope(|scope| {
        let workers: Vec<_> = (1..=threads.min(count))
            .map(|first| {
                scope.spawn(move || -> Result<Vec<_>, Error> {
                    let mut vgmstream = Vgmstream::new()?.sharing(Arc::clone(map_file));
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
            return if mission_code(file).is_some() {
                None
            } else if is_localized {
                // Despite their names, the `Music_*.LS0` banks hold Lambert's
                // alarm warnings and guards' alerts.
                Some(SoundKind::Voices)
            } else if file
                .get(..6)
                .is_some_and(|prefix| prefix.eq_ignore_ascii_case("Music_"))
            {
                Some(SoundKind::Music)
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

    /// Whether the sound has a name of its own, which only sounds stored in
    /// the map do.
    fn has_name(&self) -> bool {
        self.stream_file().is_none() && !matches!(self.entry_type, Some(SEQUENCE | LAYERS))
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
            Some(SoundKind::Voices)
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
    fn lists_each_sound_once_with_the_areas_that_play_it() {
        let sound = |stream_name: &str, sample_count| SoundInfo {
            sample_count,
            stream_name: stream_name.to_owned(),
            ..info(1)
        };
        let mut lists = SoundLists::default();
        for (subsong, info) in [
            sound("1_1_0Tbilisi/0001/01-001f0008/DOOJAM_1.wav", 100),
            sound("1_1_0Tbilisi/0002/01-00050001/ALARM_1.wav", 50),
            sound("1_1_1Tbilisi/0001/01-001f0008/DOOJAM_1.wav", 100),
            // The same name and key, but a different length.
            sound("1_1_1Tbilisi/0002/01-001f0008/DOOJAM_1.wav", 120),
            sound("menu/0001/01-001f0008/DOOJAM_1.wav", 100),
            sound("menu/0002/01-00660002/Music_Common.SS0", 100),
        ]
        .into_iter()
        .enumerate()
        {
            lists.add("MAPS.SM0", subsong as u32 + 1, false, info);
        }

        let lists = lists.into_sorted();
        let effects: Vec<_> = lists[&SoundKind::Effects]
            .iter()
            .map(|segment| {
                let SegmentSource::MapSound { areas, .. } = &segment.source else {
                    panic!("{} isn't a map sound", segment.id);
                };
                (segment.segment, segment.id.as_str(), areas.join(","))
            })
            .collect();
        assert_eq!(
            effects,
            [
                (0, "MAPS.SM0#2", "1_1_0Tbilisi".to_owned()),
                (1, "MAPS.SM0#1", "1_1_0Tbilisi,1_1_1Tbilisi,menu".to_owned()),
                (2, "MAPS.SM0#4", "1_1_1Tbilisi".to_owned()),
            ]
        );
        assert_eq!(lists[&SoundKind::Music].len(), 1);
    }

    #[test]
    fn lists_sounds_without_a_name_last() {
        let mut lists = SoundLists::default();
        for (subsong, stream_name) in [
            "0_0_2_Training/0000/01-00660002/Music_Common.LS0",
            "0_0_2_Training/0001/01-00130086/ZZZ_1.ima",
            "0_0_2_Training/0002/01-00130087/AAA_1.ima",
        ]
        .into_iter()
        .enumerate()
        {
            let info = SoundInfo {
                stream_name: stream_name.to_owned(),
                ..info(1)
            };
            lists.add("ENGLISH/MAPS.LM0", subsong as u32 + 1, true, info);
        }

        let names: Vec<_> = lists.into_sorted()[&SoundKind::Voices]
            .iter()
            .map(|segment| segment.name.clone().unwrap())
            .collect();
        assert_eq!(names, ["AAA_1", "ZZZ_1", "Music_Common.LS0"]);
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
