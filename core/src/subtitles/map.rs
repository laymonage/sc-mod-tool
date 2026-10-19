use std::collections::{HashMap, HashSet};

use crate::sound_map_file::{AUDIO, SoundMapFile, read_u32};

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct MapRecord {
    /// The key of the resource holding the audio, which the event that plays
    /// it doesn't always share.
    pub(crate) resource_key: u32,
    pub(super) offset: u32,
    pub(super) compressed_bytes: u32,
    pub(crate) sample_count: u32,
    /// Zero when the record is too short to include it.
    pub(crate) sample_rate: u32,
    /// The bank file holding the audio, such as `1_3_2.LS0`. A level can play
    /// sounds from another level's package, so this can't be inferred from the
    /// package name.
    pub(super) bank: Option<String>,
}

/// A `MAPS.LM0` and the banks it describes: the language's own folder, or one
/// of an extra map's language folders, which have their own banks and maps.
pub(super) struct SoundMap<'a> {
    /// The map, relative to `Sounds`, such as `1_7_1_2Vselka/English/MAPS.LM0`.
    pub(super) path: String,
    pub(super) records: HashMap<u32, HashSet<MapRecord>>,
    pub(super) banks: Vec<&'a crate::Bank>,
}

/// The duration of the clip a key's map record describes, from the first map
/// that has the key.
pub(super) fn clip_seconds(key: u32, maps: &[SoundMap]) -> Option<f64> {
    let record = maps
        .iter()
        .find_map(|map| map.records.get(&key))?
        .iter()
        .next()?;
    let rate = if record.sample_rate == 0 {
        36000
    } else {
        record.sample_rate
    };
    Some(f64::from(record.sample_count) / f64::from(rate))
}

/// Reads a `MAPS.LM0`'s audio, by the keys of the events that play it.
///
/// An event's key is usually its resource's too, but not always: in some
/// packages a run of events plays its resources in reverse order, for example.
/// Dialogue's events point straight at its audio, so containers, which
/// characters' barks are chosen through, aren't followed.
pub(crate) fn parse_map_records(data: &[u8]) -> Result<HashMap<u32, HashSet<MapRecord>>, String> {
    let map = SoundMapFile::parse(data)?;
    let mut records = HashMap::<u32, HashSet<MapRecord>>::new();
    for area in map.areas() {
        for event in map.events(area) {
            if let Some(record) = event
                .resource
                .and_then(|resource| read_audio_record(data, resource))
            {
                records.entry(event.key).or_default().insert(record);
            }
        }
    }
    Ok(records)
}

/// Reads the resource at `position`, if it's a clip of audio.
fn read_audio_record(data: &[u8], position: usize) -> Option<MapRecord> {
    let compressed_bytes = read_u32(data, position + 12)?;
    let sample_count = read_u32(data, position + 48)?;
    if read_u32(data, position + 4)? != AUDIO
        || read_u32(data, position + 8)? != 1
        || compressed_bytes == 0
        || sample_count == 0
        || read_u32(data, position + 52)? != compressed_bytes
    {
        return None;
    }
    Some(MapRecord {
        resource_key: read_u32(data, position)?,
        offset: read_u32(data, position + 20)?,
        compressed_bytes,
        sample_count,
        sample_rate: read_u32(data, position + 68).unwrap_or(0),
        bank: read_bank_name(data, position + 80),
    })
}

/// Reads a record's null-terminated bank file name, which is at most 32 bytes.
fn read_bank_name(data: &[u8], position: usize) -> Option<String> {
    let field = data.get(position..position.checked_add(32)?)?;
    let name = &field[..field.iter().position(|byte| *byte == 0)?];
    (!name.is_empty() && name.is_ascii()).then(|| String::from_utf8_lossy(name).into_owned())
}

/// Finds a sound's segment in every map that has it. Extra maps ship their own
/// copies of some banks, so a sound can have a segment in each.
pub(super) fn resolve_segments<'a>(
    sound_event: &str,
    key: u32,
    maps: &[SoundMap<'a>],
) -> Result<Vec<&'a crate::Segment>, String> {
    let mut segments = Vec::new();
    let mut warning = None;
    for map in maps {
        if !map.records.contains_key(&key) {
            continue;
        }
        match resolve_segment(sound_event, key, map) {
            Ok(segment) => segments.push(segment),
            Err(error) => warning = warning.or(Some(error)),
        }
    }
    if segments.is_empty() {
        Err(warning.unwrap_or_else(|| {
            format!("{sound_event} (key 0x{key:08X}) has no matching entry in MAPS.LM0.")
        }))
    } else {
        Ok(segments)
    }
}

fn resolve_segment<'a>(
    sound_event: &str,
    key: u32,
    map: &SoundMap<'a>,
) -> Result<&'a crate::Segment, String> {
    let Some(records) = map.records.get(&key) else {
        return Err(format!(
            "{sound_event} (key 0x{key:08X}) has no matching entry in MAPS.LM0."
        ));
    };

    let expected_code = sound_package_bank_code(sound_event);
    let mut matches = Vec::<&crate::Segment>::new();
    let mut add = |segment: &'a crate::Segment| {
        if !matches.iter().any(|matched| matched.id == segment.id) {
            matches.push(segment);
        }
    };
    for record in records {
        // Audio in a numeric `.LS0` bank is a stream found by scanning it.
        // Records name their bank; the package name is only a fallback.
        for bank in map.banks.iter().filter(|bank| match &record.bank {
            Some(name) => bank.name.eq_ignore_ascii_case(name),
            None => expected_code
                .as_deref()
                .is_none_or(|code| bank.code.eq_ignore_ascii_case(code)),
        }) {
            for segment in &bank.segments {
                if segment.stream_range()
                    == Some((u64::from(record.offset), u64::from(record.compressed_bytes)))
                    && segment.sample_count == record.sample_count
                {
                    add(segment);
                }
            }
        }
        // Other audio, such as Lambert's alarm warnings in
        // `Music_Common.LS0`, is listed with this map's sounds, under the key
        // of its resource.
        for bank in map
            .banks
            .iter()
            .filter(|bank| bank.sound_kind.is_some() && bank.path == map.path)
        {
            for segment in &bank.segments {
                if matches!(
                    segment.source,
                    crate::SegmentSource::MapSound { key: Some(resource_key), .. }
                        if resource_key == record.resource_key
                ) {
                    add(segment);
                }
            }
        }
    }

    match matches.as_slice() {
        [segment] => Ok(segment),
        [] => Err(format!(
            "{sound_event} (key 0x{key:08X}) did not resolve to a recognized segment in MAPS.LM0."
        )),
        _ => Err(format!(
            "{sound_event} (key 0x{key:08X}) resolves to multiple audio segments."
        )),
    }
}

fn sound_package_bank_code(sound_event: &str) -> Option<String> {
    let (package, _) = sound_event.split_once('.')?;
    let code = package
        .strip_prefix('S')?
        .strip_suffix("Voice")?
        .trim_end_matches('_');
    if code.is_empty()
        || !code
            .chars()
            .all(|character| character.is_ascii_digit() || character == '_')
    {
        return None;
    }
    Some(code.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sound_map_file::tests::{TestResource, write_u32};
    use crate::sound_map_file::{self, AREA_SIZE, EVENT_SIZE, NORMAL_PITCH};
    use crate::subtitles::tests::{test_bank, test_maps, test_records};

    /// A map with one area, whose events play the resources at the given
    /// indexes, each an audio clip with the given key at offset `key * 100`.
    fn test_map(events: &[(u32, u32)], resource_keys: &[u32]) -> Vec<u8> {
        let events: Vec<_> = events
            .iter()
            .map(|&(key, resource)| (key, resource, NORMAL_PITCH))
            .collect();
        let resources: Vec<_> = resource_keys
            .iter()
            .map(|&key| TestResource::Audio(key))
            .collect();
        sound_map_file::tests::test_map(&events, &resources)
    }

    #[test]
    fn finds_audio_through_the_events_that_play_it() {
        // Two events play each other's resources, as in `S0_0_2Voice`, and a
        // third points at no resource.
        let mut data = test_map(&[(0x3c0000, 1), (0x3c0001, 0)], &[0x3c0000, 0x3c0001]);
        let records = parse_map_records(&data).unwrap();
        let record = |key| records.get(&key).unwrap().iter().next().unwrap().clone();
        assert_eq!(
            record(0x3c0000),
            MapRecord {
                resource_key: 0x3c0001,
                offset: 0x3c0001 * 100,
                compressed_bytes: 15_571,
                sample_count: 29_967,
                sample_rate: 36_000,
                bank: Some("0_0_2.LS0".to_owned()),
            }
        );
        assert_eq!(record(0x3c0001).resource_key, 0x3c0000);

        // A resource that isn't audio is skipped.
        let resource = 0x0c + AREA_SIZE + 0x24 + 2 * EVENT_SIZE;
        write_u32(&mut data, resource + 4, 8);
        let records = parse_map_records(&data).unwrap();
        assert!(!records.contains_key(&0x3c0001));
        assert!(records.contains_key(&0x3c0000));
    }

    #[test]
    fn rejects_maps_it_cant_read() {
        assert!(parse_map_records(&[]).is_err());
        let mut data = test_map(&[(1, 0)], &[1]);
        write_u32(&mut data, 0, 8);
        assert!(parse_map_records(&data).is_err());
        let mut data = test_map(&[(1, 0)], &[1]);
        data.truncate(0x0c + AREA_SIZE + 0x10);
        assert!(parse_map_records(&data).is_err());
    }

    #[test]
    fn matches_sound_packages_to_numeric_bank_names() {
        assert_eq!(
            sound_package_bank_code("S1_1_0Voice.Play_11_04_02"),
            Some("1_1_0".to_owned())
        );
        assert_eq!(
            sound_package_bank_code("S1_1_Voice.Play_11_90_01"),
            Some("1_1".to_owned())
        );
        assert_eq!(sound_package_bank_code("FisherVoice.Play_Alarm"), None);
    }

    #[test]
    fn finds_a_sound_in_each_map_with_its_own_banks() {
        // Both maps ship a different 3_1_1.LS0, and their records point at
        // different offsets for the same key.
        let map_bank = |map: &str, offsets: &[u64]| {
            let mut bank = test_bank(offsets).remove(0);
            bank.name = "3_1_1.LS0".to_owned();
            bank.code = "3_1_1".to_owned();
            bank.map = Some(map.to_owned());
            bank.path = format!("{map}/English/3_1_1.LS0");
            for segment in &mut bank.segments {
                segment.id = format!("{}:{}", bank.path, segment.stream_range().unwrap().0);
            }
            bank
        };
        let banks = [
            map_bank("1_7_1_1VselkaInfiltration", &[0, 100]),
            map_bank("1_7_1_2Vselka", &[0, 100]),
        ];
        let map = |folder: &str, records, banks| SoundMap {
            path: format!("{folder}/MAPS.LM0"),
            records,
            banks,
        };
        let maps = [
            map(
                "1_7_1_1VselkaInfiltration/English",
                test_records(&[(7, 0)]),
                vec![&banks[0]],
            ),
            map(
                "1_7_1_2Vselka/English",
                test_records(&[(7, 100)]),
                vec![&banks[1]],
            ),
            map("ENGLISH", HashMap::new(), Vec::new()),
        ];

        let segments = resolve_segments("S3_1_1Voice.Play_31_01_01", 7, &maps).unwrap();

        assert_eq!(
            segments
                .iter()
                .map(|segment| segment.id.as_str())
                .collect::<Vec<_>>(),
            [
                "1_7_1_1VselkaInfiltration/English/3_1_1.LS0:0",
                "1_7_1_2Vselka/English/3_1_1.LS0:100",
            ]
        );
        assert_eq!(
            resolve_segments("S3_1_1Voice.Play_31_01_01", 8, &maps).unwrap_err(),
            "S3_1_1Voice.Play_31_01_01 (key 0x00000008) has no matching entry in MAPS.LM0."
        );
    }

    #[test]
    fn finds_audio_in_the_bank_its_map_record_names() {
        // Level 1_3_2 plays a sound from level 1_3_3's package.
        let banks = test_bank(&[0, 100]);
        let mut records = test_records(&[(7, 100)]);
        let record = records.remove(&7).unwrap().into_iter().next().unwrap();
        records.insert(
            7,
            HashSet::from([MapRecord {
                bank: Some("1_1_2.LS0".to_owned()),
                ..record
            }]),
        );

        let maps = test_maps(records, &banks);
        let segment = resolve_segment("S1_3_3Voice.Play_13_21_01", 7, &maps[0]).unwrap();

        assert_eq!(segment.id, "ENGLISH/1_1_2.LS0:100");
    }

    #[test]
    fn finds_audio_listed_with_the_maps_sounds() {
        // Lambert's alarm warning is in `Music_Common.LS0`, which isn't
        // scanned. The map's voices list it under its resource's key.
        let mut banks = test_bank(&[0]);
        let mut voices = banks[0].clone();
        voices.name = "Voices".to_owned();
        voices.sound_kind = Some(crate::SoundKind::Voices);
        voices.path = "ENGLISH/MAPS.LM0".to_owned();
        voices.segments[0].id = "ENGLISH/MAPS.LM0#1".to_owned();
        voices.segments[0].source = crate::SegmentSource::MapSound {
            subsong: 1,
            key: Some(0x660002),
            areas: Vec::new(),
            pitch: NORMAL_PITCH,
        };
        banks.push(voices);
        let mut records = test_records(&[(0x660005, 0)]);
        let record = records
            .remove(&0x660005)
            .unwrap()
            .into_iter()
            .next()
            .unwrap();
        records.insert(
            0x660005,
            HashSet::from([MapRecord {
                resource_key: 0x660002,
                bank: Some("Music_Common.LS0".to_owned()),
                ..record
            }]),
        );

        let maps = test_maps(records, &banks);
        let segment = resolve_segment("Lambert.Play_AlarmStage1Opt1", 0x660005, &maps[0]).unwrap();

        assert_eq!(segment.id, "ENGLISH/MAPS.LM0#1");
    }
}
