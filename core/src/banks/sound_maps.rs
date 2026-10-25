//! The sounds in the game's sound maps, which vgmstream reads.
//!
//! `MAPS.SM0` holds the sound effects and music every language shares, and
//! each language's `MAPS.LM0` the localized ones. A map is split into areas,
//! one for each level plus `fix` and `menu`, and lists every sound an area
//! plays. Some sounds' audio is in the map itself; others point at a stream
//! file such as `Music_Common.SS0`, `STREAM.SS0` or a level's `.LS0` bank.
//! The extra maps have their own `MAPS.SM0` and `MAPS.LM0`.

use std::collections::hash_map::Entry;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use super::missions::{leading_mission_code, mission_code};
use crate::sound_map_file::{
    NORMAL_PITCH, SoundMapFile, UBISOFT_ADPCM, audio_resource, file_name, pitched_sample_rate,
    read_u32,
};
use crate::sounds_folder::find_file;
use crate::vgmstream::{SharedFile, SoundInfo, Vgmstream};
use crate::{Area, Bank, Error, FileStream, Segment, SegmentSource, StreamFormat};

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
    let mut lists = SoundLists {
        pitches: SoundMapFile::parse(map_file.bytes())
            .map(|file| file.clip_pitches())
            .unwrap_or_default(),
        ..SoundLists::default()
    };
    let mut failures = 0;
    for (subsong, info) in open_sounds(sounds, path, &map_file, sound_count, cancelled)? {
        match info {
            Some(info) => lists.add(path, subsong, is_localized, info),
            None => failures += 1,
        }
    }

    let mut lists = lists.into_sorted();
    let streams = file_streams(map_file.bytes());
    let mut segments: Vec<_> = lists.values_mut().flatten().collect();
    add_file_streams(sounds, path, &streams, &mut segments);
    let mut banks: Vec<_> = lists
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

/// A sound's audio that can be replaced, as the map's resources describe it.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Replaceable {
    /// Where it starts in its file: once in an `.LS0` file, and once for each
    /// area's copy of a clip stored in the map.
    offsets: Vec<u64>,
    /// The areas the copies are in, by index.
    areas: Vec<usize>,
    length: u64,
    sample_count: u32,
    /// The rate it's stored at.
    rate: Option<u32>,
    format: StreamFormat,
    /// The sample its loop starts at, if it loops.
    loop_start: Option<u32>,
}

/// The audio the map `map` describes that can be replaced, by the key of the
/// resources describing it and the lowercase name the sound is listed by:
///
/// - Ubisoft ADPCM streams in `.LS0` files other than numeric banks, listed
///   by the file's name. Every resource with the key and file must agree on
///   where the stream is.
/// - Mono 16-bit PCM clips stored in the map, listed by their name without
///   its extension. Each area keeps a copy, and the copies must be the same.
///
/// `None` where the resources don't meet that, one of them is another kind of
/// resource or audio, or another sound plays the same audio.
fn file_streams(map: &[u8]) -> HashMap<(u32, String), Option<Replaceable>> {
    let mut streams = HashMap::new();
    let Ok(file) = SoundMapFile::parse(map) else {
        return streams;
    };
    // Which sounds play the audio at each place, by its file's lowercase name,
    // empty for the map itself, and offset.
    let mut players = HashMap::<(String, u64), HashSet<(u32, String)>>::new();
    let mut add =
        |key: u32, name: String, found: Option<Replaceable>| match streams.entry((key, name)) {
            Entry::Occupied(mut known) => merge(map, known.get_mut(), found),
            Entry::Vacant(entry) => {
                entry.insert(found);
            }
        };
    for (area_index, area) in file.areas().iter().enumerate() {
        let clips: HashMap<_, _> = file
            .stored_clips(area)
            .into_iter()
            .map(|clip| (clip.resource, clip))
            .collect();
        for resource in file.resources(area) {
            let Some(audio) = audio_resource(map, resource) else {
                // Another kind of resource, which may keep a file name left
                // over from the resource before it.
                if let (Some(key), Some(name)) = (read_u32(map, resource), file_name(map, resource))
                {
                    let name = String::from_utf8_lossy(name).to_ascii_lowercase();
                    add(key, stem(&name).to_owned(), None);
                    add(key, name, None);
                }
                continue;
            };
            let name = String::from_utf8_lossy(audio.file_name).to_ascii_lowercase();
            if name.ends_with(".ls0") {
                if mission_code(&name).is_some() {
                    continue;
                }
                if let Some(placement) = audio.placement {
                    players
                        .entry((name.clone(), placement.offset))
                        .or_default()
                        .insert((audio.key, name.clone()));
                }
                let found = audio
                    .placement
                    .filter(|_| audio.codec == Some(UBISOFT_ADPCM))
                    .map(|placement| Replaceable {
                        offsets: vec![placement.offset],
                        areas: vec![area_index],
                        length: placement.length,
                        sample_count: placement.sample_count,
                        rate: audio.rate,
                        format: StreamFormat::UbisoftAdpcm,
                        loop_start: audio.loop_start,
                    });
                add(audio.key, name, found);
            } else if !name.ends_with(".ss0") {
                let identity = (audio.key, stem(&name).to_owned());
                let clip = clips.get(&resource);
                if let Some(clip) = clip {
                    players
                        .entry((String::new(), clip.offset))
                        .or_default()
                        .insert(identity.clone());
                }
                let found = clip
                    .filter(|clip| {
                        clip.channels == 1 && clip.length == u64::from(clip.sample_count) * 2
                    })
                    .map(|clip| Replaceable {
                        offsets: vec![clip.offset],
                        areas: vec![area_index],
                        length: clip.length,
                        sample_count: clip.sample_count,
                        rate: audio.rate,
                        format: StreamFormat::Pcm16,
                        loop_start: clip.loop_start,
                    });
                add(identity.0, identity.1, found);
            }
        }
    }
    // Replacing audio more than one sound plays would change them all.
    for sounds in players.into_values().filter(|sounds| sounds.len() > 1) {
        for sound in sounds {
            streams.insert(sound, None);
        }
    }
    streams
}

/// `name` without its extension.
fn stem(name: &str) -> &str {
    name.rsplit_once('.').map_or(name, |(stem, _)| stem)
}

/// Adds what another resource in the map `map` says about a sound's audio to
/// what's `known`.
fn merge(map: &[u8], known: &mut Option<Replaceable>, found: Option<Replaceable>) {
    *known = match (known.take(), found) {
        // Another area's copy of a clip, the same as the first.
        (Some(mut known), Some(found))
            if known.format == StreamFormat::Pcm16
                && (
                    found.format,
                    found.length,
                    found.sample_count,
                    found.rate,
                    found.loop_start,
                ) == (
                    known.format,
                    known.length,
                    known.sample_count,
                    known.rate,
                    known.loop_start,
                )
                && found.areas.iter().all(|area| !known.areas.contains(area))
                && same_bytes(map, known.offsets[0], found.offsets[0], known.length) =>
        {
            known.offsets.extend(found.offsets);
            known.areas.extend(found.areas);
            Some(known)
        }
        (Some(known), Some(found))
            if known.format == StreamFormat::UbisoftAdpcm
                && (
                    &known.offsets,
                    known.length,
                    known.sample_count,
                    known.rate,
                    known.format,
                    known.loop_start,
                ) == (
                    &found.offsets,
                    found.length,
                    found.sample_count,
                    found.rate,
                    found.format,
                    found.loop_start,
                ) =>
        {
            Some(known)
        }
        _ => None,
    };
}

/// Whether `map` has the same `length` bytes at `first` and `second`.
fn same_bytes(map: &[u8], first: u64, second: u64, length: u64) -> bool {
    let bytes = |offset: u64| map.get(offset as usize..(offset + length) as usize);
    bytes(first).is_some() && bytes(first) == bytes(second)
}

/// Notes where each of `segments`' audio is, sounds in the map at `path`, if
/// it's in `streams`: a stream in an `.LS0` file beside the map, or a clip
/// stored in the map. Audio two sounds play isn't noted for either, so that
/// replacing a sound never changes another.
fn add_file_streams(
    sounds: &Path,
    path: &str,
    streams: &HashMap<(u32, String), Option<Replaceable>>,
    segments: &mut [&mut Segment],
) {
    let folder = path.rsplit_once('/').map_or("", |(folder, _)| folder);
    // Each `.LS0` file's path relative to `Sounds`, by its lowercase name.
    let mut files = HashMap::<String, Option<String>>::new();
    for segment in segments.iter_mut() {
        let SegmentSource::MapSound {
            key: Some(key),
            stream,
            ..
        } = &mut segment.source
        else {
            continue;
        };
        // A sound in a stream file is named after the file, and one stored in
        // the map after its recording.
        let Some(name) = segment.name.as_deref().map(str::to_ascii_lowercase) else {
            continue;
        };
        let Some(Some(found)) = streams.get(&(*key, name.clone())) else {
            continue;
        };
        if found.sample_count != segment.sample_count {
            continue;
        }
        let file = match found.format {
            StreamFormat::Pcm16 => Some(path.to_owned()),
            StreamFormat::UbisoftAdpcm => files
                .entry(name)
                .or_insert_with_key(|name| {
                    let found = find_file(&sounds.join(folder), name).ok()?;
                    let name = found.file_name()?.to_str()?;
                    Some(if folder.is_empty() {
                        name.to_owned()
                    } else {
                        format!("{folder}/{name}")
                    })
                })
                .clone(),
        };
        if let Some(file) = file {
            let mut offsets = found.offsets.clone();
            offsets.sort_unstable();
            *stream = Some(FileStream {
                path: file,
                offsets,
                byte_length: found.length,
                format: found.format,
                loop_start: found.loop_start,
            });
        }
    }

    let places = |segment: &Segment| match &segment.source {
        SegmentSource::MapSound {
            stream: Some(stream),
            ..
        } => stream
            .offsets
            .iter()
            .map(|&offset| (stream.path.clone(), offset))
            .collect(),
        _ => Vec::new(),
    };
    let mut counts = HashMap::<(String, u64), usize>::new();
    for place in segments.iter().flat_map(|segment| places(segment)) {
        *counts.entry(place).or_default() += 1;
    }
    for segment in segments.iter_mut() {
        if places(segment).iter().any(|place| counts[place] > 1)
            && let SegmentSource::MapSound { stream, .. } = &mut segment.source
        {
            *stream = None;
        }
    }
}

/// The area named `name`. Its mission name depends on the language, so it's
/// added after the scan.
fn area(name: &str) -> Area {
    Area {
        name: name.to_owned(),
        code: leading_mission_code(name),
        mission_name: None,
    }
}

/// A map's sounds, in one list for each kind.
///
/// Each of the map's areas lists every sound it plays, so most sounds are
/// listed by several areas. Entries with the same key, name and length are the
/// same sound, so it's listed once, with every area that plays it.
#[derive(Default)]
struct SoundLists<'a> {
    lists: BTreeMap<SoundKind, Vec<Segment>>,
    /// Where each sound with a key is in its list.
    listed: HashMap<(SoundKind, u32, String, u32), usize>,
    /// The IDs of sounds without a name of their own, which are listed by
    /// their stream file or kind of entry instead.
    unnamed: HashSet<String>,
    /// The pitch the map's events play its clips at, by area name and
    /// resource index, from [`SoundMapFile::clip_pitches`].
    pitches: HashMap<(&'a [u8], usize), u32>,
}

impl SoundLists<'_> {
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
                && !areas.iter().any(|area| area.name == entry.area)
            {
                areas.push(area(entry.area));
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
        let pitch = entry
            .index
            .and_then(|index| self.pitches.get(&(entry.area.as_bytes(), index)))
            .copied()
            .unwrap_or(NORMAL_PITCH);
        segments.push(Segment {
            id,
            source: SegmentSource::MapSound {
                subsong,
                key: entry.key,
                areas: [entry.area]
                    .into_iter()
                    .filter(|area| !area.is_empty())
                    .map(area)
                    .collect(),
                stream: None,
                pitch,
            },
            name: Some(name),
            sample_count: info.sample_count,
            sample_rate: pitched_sample_rate(info.sample_rate, pitch),
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
/// `1_1_0Tbilisi/0012/01-00130086/TAWCHI02.wav`: the area, the entry's index
/// in the area's resources, its type and sound key, and the sound's name or
/// the file holding its audio.
#[derive(Debug, PartialEq, Eq)]
struct MapEntry<'a> {
    area: &'a str,
    index: Option<usize>,
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
        let has_area = parts.len() == 4;
        MapEntry {
            area: if has_area { parts[0] } else { "" },
            index: has_area.then(|| parts[1].parse().ok()).flatten(),
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
                index: Some(12),
                entry_type: Some(0x01),
                key: Some(0x0013_0086),
                name: "TAWCHI02.wav",
            }
        );
        let unexpected = MapEntry::parse("TAWCHI02.wav");
        assert_eq!(unexpected.area, "");
        assert_eq!((unexpected.index, unexpected.key), (None, None));
        assert_eq!(unexpected.display_name(), "TAWCHI02");
    }

    #[test]
    fn finds_streams_in_ls0_files() {
        use crate::sound_map_file::tests::{test_map, write_u32};
        use crate::sound_map_file::{AREA_SIZE, CODEC, OFFSET, RESOURCE_SIZE};

        // Resources by key, file, codec, and type if not audio.
        let resources: [(u32, &str, u32, Option<u32>); 8] = [
            (1, "Music_Common.LS0", UBISOFT_ADPCM, None),
            // The same key twice, at different streams.
            (2, "Music_Common.LS0", UBISOFT_ADPCM, None),
            (2, "Music_Common.LS0", UBISOFT_ADPCM, None),
            // The same key in another codec.
            (3, "Music_Common.LS0", UBISOFT_ADPCM, None),
            (3, "Music_Common.LS0", 4, None),
            // The same key in a switch, with a leftover file name.
            (4, "Music_Common.LS0", UBISOFT_ADPCM, None),
            (4, "Music_Common.LS0", UBISOFT_ADPCM, Some(8)),
            (5, "1_1_0.LS0", UBISOFT_ADPCM, None),
        ];
        let mut map = test_map(&[], &resources.map(|(key, ..)| key));
        let first = 0x0c + AREA_SIZE + 0x24;
        for (index, (_, name, codec, kind)) in resources.into_iter().enumerate() {
            let resource = first + index * RESOURCE_SIZE;
            let name = format!("{name}\0");
            map[resource + 80..resource + 80 + name.len()].copy_from_slice(name.as_bytes());
            write_u32(&mut map, resource + CODEC, codec);
            write_u32(&mut map, resource + OFFSET, index as u32 * 1000);
            if let Some(kind) = kind {
                write_u32(&mut map, resource + 4, kind);
            }
        }
        let streams = file_streams(&map);
        let common = |key| streams.get(&(key, "music_common.ls0".to_owned())).cloned();
        let first = Replaceable {
            offsets: vec![0],
            areas: vec![0],
            length: 15_571,
            sample_count: 29_967,
            rate: Some(36_000),
            format: StreamFormat::UbisoftAdpcm,
            loop_start: None,
        };
        assert_eq!(common(1), Some(Some(first.clone())));
        for key in [2, 3, 4] {
            assert_eq!(common(key), Some(None), "{key}");
        }
        // Numeric banks' streams are found by scanning them instead. The
        // switch rules out its leftover name with and without its extension.
        assert_eq!(streams.len(), 5);

        // Two sounds playing one stream can't be replaced.
        let sound = |key, name: &str| Segment {
            id: format!("ENGLISH/MAPS.LM0#{key}"),
            source: SegmentSource::MapSound {
                subsong: key,
                key: Some(key),
                areas: Vec::new(),
                stream: None,
                pitch: NORMAL_PITCH,
            },
            name: Some(name.to_owned()),
            sample_count: 29_967,
            sample_rate: 36_000,
            channels: 1,
            codec: String::new(),
            segment: 0,
        };
        let game = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(game.path().join("ENGLISH")).unwrap();
        std::fs::write(game.path().join("ENGLISH/Music_Common.LS0"), b"").unwrap();
        let at = |offset| {
            Some(Replaceable {
                offsets: vec![offset],
                ..first.clone()
            })
        };
        let streams = HashMap::from([
            ((1, "music_common.ls0".to_owned()), at(0)),
            ((6, "music_common.ls0".to_owned()), at(0)),
            ((7, "music_common.ls0".to_owned()), at(2000)),
        ]);
        let mut sounds = [
            sound(1, "Music_Common.LS0"),
            sound(6, "MUSIC_COMMON.LS0"),
            sound(7, "Music_Common.LS0"),
        ];
        let mut segments: Vec<_> = sounds.iter_mut().collect();
        add_file_streams(game.path(), "ENGLISH/MAPS.LM0", &streams, &mut segments);
        let found: Vec<_> = sounds
            .iter()
            .map(|sound| {
                let stream = sound.replaceable_stream("ENGLISH/MAPS.LM0")?;
                Some((stream.path, stream.offsets.to_vec()))
            })
            .collect();
        assert_eq!(
            found,
            [None, None, Some(("ENGLISH/Music_Common.LS0", vec![2000]))]
        );
    }

    #[test]
    fn finds_clips_stored_in_the_map_only_when_their_copies_agree() {
        use crate::sound_map_file::tests::{map_with_clips, write_u32};
        use crate::sound_map_file::{AREA_SIZE, RESOURCE_SIZE, SIZE, SIZE_AGAIN};

        let pcm = |streams: &HashMap<(u32, String), Option<Replaceable>>, key, name: &str| {
            streams
                .get(&(key, name.to_owned()))
                .cloned()
                .map(|found| found.map(|found| found.offsets.len()))
        };
        // In both areas, the same.
        let map = map_with_clips(
            &[
                &[(1, "DOOR.wav", &[1, 2]), (2, "STEP.wav", &[3, 4])],
                &[(1, "DOOR.wav", &[1, 2]), (2, "STEP.wav", &[3, 5])],
            ],
            &[],
        );
        let streams = file_streams(&map);
        assert_eq!(pcm(&streams, 1, "door"), Some(Some(2)));
        // The copies differ.
        assert_eq!(pcm(&streams, 2, "step"), Some(None));

        // Twice in one area.
        let map = map_with_clips(
            &[&[(1, "DOOR.wav", &[1, 2]), (1, "DOOR.wav", &[1, 2])]],
            &[],
        );
        assert_eq!(pcm(&file_streams(&map), 1, "door"), Some(None));

        // Two sounds playing one clip, and one clip of the wrong length.
        let mut map = map_with_clips(
            &[&[
                (1, "DOOR.wav", &[1, 2]),
                (2, "STEP.wav", &[3, 4]),
                (3, "BELL.wav", &[5, 6]),
            ]],
            &[],
        );
        let header = 0x0c + AREA_SIZE;
        let section = header + 0x24 + 3 * RESOURCE_SIZE;
        write_u32(&mut map, section + 0x14 + 8 + 4, 0);
        let bell = header + 0x24 + 2 * RESOURCE_SIZE;
        write_u32(&mut map, bell + SIZE, 2);
        write_u32(&mut map, bell + SIZE_AGAIN, 2);
        let streams = file_streams(&map);
        assert_eq!(pcm(&streams, 1, "door"), Some(None));
        assert_eq!(pcm(&streams, 2, "step"), Some(None));
        assert_eq!(pcm(&streams, 3, "bell"), Some(None));
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
    fn reads_level_codes_from_area_names() {
        assert_eq!(area("1_1_0Tbilisi").code.as_deref(), Some("1_1_0"));
        assert_eq!(area("2_2_1_Kalinatek").code.as_deref(), Some("2_2_1"));
        assert_eq!(area("menu").code, None);
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
                let areas: Vec<_> = areas.iter().map(|area| area.name.as_str()).collect();
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
