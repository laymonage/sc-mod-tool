use std::collections::{HashMap, HashSet};

use super::push_warning;
use crate::sound_keys::PackageKeys;

/// The bytes holding the fields every record needs. Records are 128 bytes, but
/// the sample rate and bank name after these are optional.
const MAP_RECORD_REQUIRED_SIZE: usize = 56;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct MapRecord {
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
    /// The folder, relative to `Sounds`, such as `1_7_1_2Vselka/English`.
    pub(super) folder: String,
    pub(super) records: HashMap<u32, HashSet<MapRecord>>,
    pub(super) banks: Vec<&'a crate::Bank>,
    /// IDs of packages this map was made for another version of, whose keys
    /// point at other audio here.
    pub(super) foreign_packages: HashSet<u32>,
}

/// Finds the packages `map` was made for another version of: most of its
/// records under the package's ID are for sounds the package doesn't have. The
/// extra Vselka maps were made this way for `S3_1_1Voice`, so the game's keys
/// for that package point at other lines there.
pub(super) fn mark_foreign_packages(
    map: &mut SoundMap,
    packages: &[PackageKeys],
    warnings: &mut Vec<String>,
) {
    for package in packages {
        let (mut known, mut unknown) = (0, 0);
        for key in map.records.keys().filter(|key| *key >> 16 == package.id) {
            if package.keys.contains(key) {
                known += 1;
            } else {
                unknown += 1;
            }
        }
        if unknown > known {
            map.foreign_packages.insert(package.id);
            push_warning(
                warnings,
                format!(
                    "{}/MAPS.LM0 was made for another version of {}: {unknown} of its {} sounds for that package aren't in {}.uax, so the package's subtitles aren't matched there.",
                    map.folder,
                    package.package,
                    known + unknown,
                    package.package
                ),
            );
        }
    }
}

/// The duration of the clip a key's map record describes, from the first map
/// that has the key.
pub(super) fn clip_seconds(key: u32, maps: &[SoundMap]) -> Option<f64> {
    let record = maps
        .iter()
        .filter(|map| !map.foreign_packages.contains(&(key >> 16)))
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

pub(crate) fn parse_map_records(data: &[u8]) -> HashMap<u32, HashSet<MapRecord>> {
    let mut records = HashMap::<u32, HashSet<MapRecord>>::new();
    if data.len() < MAP_RECORD_REQUIRED_SIZE {
        return records;
    }

    for position in (0..=data.len() - MAP_RECORD_REQUIRED_SIZE).step_by(4) {
        let Some(key) = read_u32(data, position) else {
            continue;
        };
        if read_u32(data, position + 4) != Some(1) || read_u32(data, position + 8) != Some(1) {
            continue;
        }

        let Some(compressed_bytes) = read_u32(data, position + 12) else {
            continue;
        };
        let Some(offset) = read_u32(data, position + 20) else {
            continue;
        };
        let Some(sample_count) = read_u32(data, position + 48) else {
            continue;
        };
        if compressed_bytes == 0
            || sample_count == 0
            || read_u32(data, position + 52) != Some(compressed_bytes)
        {
            continue;
        }

        records.entry(key).or_default().insert(MapRecord {
            offset,
            compressed_bytes,
            sample_count,
            sample_rate: read_u32(data, position + 68).unwrap_or(0),
            bank: read_bank_name(data, position + 80),
        });
    }
    records
}

/// Reads a record's null-terminated bank file name, which is at most 32 bytes.
fn read_bank_name(data: &[u8], position: usize) -> Option<String> {
    let field = data.get(position..position.checked_add(32)?)?;
    let name = &field[..field.iter().position(|byte| *byte == 0)?];
    (!name.is_empty() && name.is_ascii()).then(|| String::from_utf8_lossy(name).into_owned())
}

fn read_u32(data: &[u8], position: usize) -> Option<u32> {
    let bytes = data.get(position..position.checked_add(4)?)?;
    Some(u32::from_le_bytes(bytes.try_into().ok()?))
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
    let mut in_foreign_map = false;
    for map in maps {
        if !map.records.contains_key(&key) {
            continue;
        }
        if map.foreign_packages.contains(&(key >> 16)) {
            in_foreign_map = true;
            continue;
        }
        match resolve_segment(sound_event, key, &map.records, &map.banks) {
            Ok(segment) => segments.push(segment),
            Err(error) => warning = warning.or(Some(error)),
        }
    }
    if segments.is_empty() {
        Err(warning.unwrap_or_else(|| {
            if in_foreign_map {
                format!(
                    "{sound_event} (key 0x{key:08X}) is only in maps made for another version of its package."
                )
            } else {
                format!("{sound_event} (key 0x{key:08X}) has no matching entry in MAPS.LM0.")
            }
        }))
    } else {
        Ok(segments)
    }
}

fn resolve_segment<'a>(
    sound_event: &str,
    key: u32,
    map_records: &HashMap<u32, HashSet<MapRecord>>,
    banks: &[&'a crate::Bank],
) -> Result<&'a crate::Segment, String> {
    let Some(records) = map_records.get(&key) else {
        return Err(format!(
            "{sound_event} (key 0x{key:08X}) has no matching entry in MAPS.LM0."
        ));
    };

    let expected_code = sound_package_bank_code(sound_event);
    let mut matches = Vec::new();
    for record in records {
        // Records name their bank; the package name is only a fallback.
        for bank in banks.iter().filter(|bank| match &record.bank {
            Some(name) => bank.name.eq_ignore_ascii_case(name),
            None => expected_code
                .as_deref()
                .is_none_or(|code| bank.code.eq_ignore_ascii_case(code)),
        }) {
            for segment in &bank.segments {
                if segment.offset == u64::from(record.offset)
                    && segment.sample_count == record.sample_count
                    && segment.byte_length == u64::from(record.compressed_bytes)
                    && !matches
                        .iter()
                        .any(|matched: &&crate::Segment| matched.id == segment.id)
                {
                    matches.push(segment);
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
    use crate::subtitles::tests::{test_bank, test_records};

    #[test]
    fn parses_map_record_key_offset_size_and_samples() {
        let mut data = vec![0; 128];
        write_u32(&mut data, 0, 0x0084_0001);
        write_u32(&mut data, 4, 1);
        write_u32(&mut data, 8, 1);
        write_u32(&mut data, 12, 15_571);
        write_u32(&mut data, 20, 173_296);
        write_u32(&mut data, 48, 29_967);
        write_u32(&mut data, 52, 15_571);
        write_u32(&mut data, 68, 36_000);
        data[80..89].copy_from_slice(b"0_0_2.LS0");

        let records = parse_map_records(&data);
        assert_eq!(
            records.get(&0x0084_0001),
            Some(&HashSet::from([MapRecord {
                offset: 173_296,
                compressed_bytes: 15_571,
                sample_count: 29_967,
                sample_rate: 36_000,
                bank: Some("0_0_2.LS0".to_owned()),
            }]))
        );
    }

    #[test]
    fn ignores_malformed_or_unaligned_map_records() {
        let mut data = vec![0; MAP_RECORD_REQUIRED_SIZE + 8];
        write_u32(&mut data, 1, 0x0084_0001);
        write_u32(&mut data, 5, 1);
        write_u32(&mut data, 9, 1);
        write_u32(&mut data, 13, 15_571);
        write_u32(&mut data, 21, 173_296);
        write_u32(&mut data, 49, 29_967);
        write_u32(&mut data, 53, 15_571);

        assert!(parse_map_records(&data).is_empty());
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
                segment.id = format!("{}:{}", bank.path, segment.offset);
            }
            bank
        };
        let banks = [
            map_bank("1_7_1_1VselkaInfiltration", &[0, 100]),
            map_bank("1_7_1_2Vselka", &[0, 100]),
        ];
        let map = |folder: &str, records, banks| SoundMap {
            folder: folder.to_owned(),
            records,
            banks,
            foreign_packages: HashSet::new(),
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
    fn skips_packages_a_map_was_made_for_another_version_of() {
        // The package has sounds 0x450000-0x450002. The extra map files three
        // sounds under its ID, two of which the package doesn't have, so its
        // key 0x450001 means some other line there.
        let banks = test_bank(&[0, 100, 200]);
        let package = PackageKeys {
            package: "S1_1_2Voice".to_owned(),
            id: 0x45,
            keys: HashSet::from([0x450000, 0x450001, 0x450002]),
        };
        let mut maps = vec![
            SoundMap {
                folder: "ENGLISH".to_owned(),
                records: test_records(&[(0x450000, 0), (0x450001, 100)]),
                banks: vec![&banks[0]],
                foreign_packages: HashSet::new(),
            },
            SoundMap {
                folder: "1_7_1_2Vselka/English".to_owned(),
                records: test_records(&[(0x450001, 100), (0x450070, 0), (0x450071, 200)]),
                banks: vec![&banks[0]],
                foreign_packages: HashSet::new(),
            },
        ];
        let mut warnings = Vec::new();

        for map in &mut maps {
            mark_foreign_packages(map, std::slice::from_ref(&package), &mut warnings);
        }

        assert!(maps[0].foreign_packages.is_empty());
        assert_eq!(maps[1].foreign_packages, HashSet::from([0x45]));
        assert_eq!(
            warnings,
            [
                "1_7_1_2Vselka/English/MAPS.LM0 was made for another version of S1_1_2Voice: 2 of its 3 sounds for that package aren't in S1_1_2Voice.uax, so the package's subtitles aren't matched there."
            ]
        );
        let segments = resolve_segments("S1_1_2Voice.Play_11_38_02", 0x450001, &maps).unwrap();
        assert_eq!(
            segments
                .iter()
                .map(|segment| segment.id.as_str())
                .collect::<Vec<_>>(),
            ["ENGLISH/1_1_2.LS0:100"]
        );
        maps.remove(0);
        assert_eq!(
            resolve_segments("S1_1_2Voice.Play_11_38_02", 0x450001, &maps).unwrap_err(),
            "S1_1_2Voice.Play_11_38_02 (key 0x00450001) is only in maps made for another version of its package."
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

        let segment = resolve_segment(
            "S1_3_3Voice.Play_13_21_01",
            7,
            &records,
            &banks.iter().collect::<Vec<_>>(),
        )
        .unwrap();

        assert_eq!(segment.id, "ENGLISH/1_1_2.LS0:100");
    }

    fn write_u32(data: &mut [u8], position: usize, value: u32) {
        data[position..position + 4].copy_from_slice(&value.to_le_bytes());
    }
}
