//! Corrects sound map keys that some of the game's sound packages store wrongly.
//!
//! Each `Sound` export stores the key of its audio in `MAPS.LM0`, but in a few
//! packages those keys are scrambled in small, local ways: a run of
//! consecutive keys is stored in reverse, two neighbouring exports have each
//! other's keys, a sound points next to the key it should use, or a sound added
//! late points at a key with no audio while its audio sits under a free key.
//! The game plays the right audio regardless, and lip-sync files (which record
//! each sound's duration) agree with what it plays, so they're used here to
//! detect and undo the first three kinds. The last is undone only when the
//! package has exactly one such sound and one such clip.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::Path;

use serde::Deserialize;

use crate::subtitles::{find_file, parse_map_records};

/// How far apart, in seconds, a lip-sync duration and a clip can be to match.
const DURATION_TOLERANCE: f64 = 0.15;

/// The sounds of one package, as reported by the UELib bridge.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SoundPackage {
    package: String,
    sounds: Vec<PackageSound>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PackageSound {
    name: String,
    export_index: u32,
    sound_map_key: u32,
    #[serde(default)]
    lip_sync_path: Option<String>,
}

/// The keys a package's sounds use, after corrections, by package ID (the
/// top 16 bits of a key).
#[derive(Debug)]
pub(crate) struct PackageKeys {
    pub(crate) package: String,
    pub(crate) id: u32,
    pub(crate) keys: HashSet<u32>,
}

/// Lists the keys each package's sounds use, after `corrections`.
pub(crate) fn package_keys(
    packages: &[SoundPackage],
    corrections: &KeyCorrections,
) -> Vec<PackageKeys> {
    let mut by_id = Vec::<PackageKeys>::new();
    for package in packages {
        for sound in &package.sounds {
            let key = corrections
                .key(&format!("{}.{}", package.package, sound.name))
                .unwrap_or(sound.sound_map_key);
            let id = key >> 16;
            match by_id
                .iter_mut()
                .find(|keys| keys.id == id && keys.package == package.package)
            {
                Some(keys) => {
                    keys.keys.insert(key);
                }
                None => by_id.push(PackageKeys {
                    package: package.package.clone(),
                    id,
                    keys: HashSet::from([key]),
                }),
            }
        }
    }
    by_id
}

/// Corrected keys, by lowercased `Package.Sound` name.
#[derive(Debug, Default)]
pub(crate) struct KeyCorrections {
    keys: HashMap<String, u32>,
    pub(crate) notes: Vec<String>,
}

impl KeyCorrections {
    pub(crate) fn key(&self, sound_event: &str) -> Option<u32> {
        self.keys.get(&sound_event.to_ascii_lowercase()).copied()
    }
}

/// Finds corrections for every package, using the English lip-sync files and
/// the English map, the only language the game ships lip-sync data for.
///
/// `played` holds the lowercased `Package.Sound` names that scripts play.
pub(crate) fn correct(
    game_root: &Path,
    sounds: &Path,
    packages: &[SoundPackage],
    played: &HashSet<String>,
) -> KeyCorrections {
    let mut corrections = KeyCorrections::default();
    let Some(lip_sync_root) = find_file(game_root, "LIPSYNCH")
        .and_then(|root| find_file(&root, "int"))
        .ok()
    else {
        return corrections;
    };
    let Some(mut durations) = find_file(sounds, "ENGLISH")
        .ok()
        .and_then(|english| map_clip_durations(&english))
    else {
        return corrections;
    };
    // Extra maps have the only copies of some packages' audio, such as
    // mission 3's.
    for folder in crate::map_language_folders(sounds, "ENGLISH").unwrap_or_default() {
        for (key, seconds) in
            map_clip_durations(&sounds.join(&folder.relative_path)).unwrap_or_default()
        {
            durations.entry(key).or_insert(seconds);
        }
    }

    for package in packages {
        let candidates = package
            .sounds
            .iter()
            .map(|sound| Candidate {
                name: &sound.name,
                export_index: sound.export_index,
                key: sound.sound_map_key,
                lip_sync: sound
                    .lip_sync_path
                    .as_deref()
                    .and_then(|path| lip_sync_duration(&lip_sync_root, path)),
                played: played
                    .contains(&format!("{}.{}", package.package, sound.name).to_ascii_lowercase()),
            })
            .collect::<Vec<_>>();
        let (keys, changes) = correct_package(&candidates, &durations);
        for (sound, key) in package.sounds.iter().zip(keys) {
            if key != sound.sound_map_key {
                corrections.keys.insert(
                    format!("{}.{}", package.package, sound.name).to_ascii_lowercase(),
                    key,
                );
            }
        }
        corrections.notes.extend(
            changes
                .into_iter()
                .map(|change| change.describe(&package.package)),
        );
    }
    corrections
}

/// Reads the clip durations from the `MAPS.LM0` in `folder`.
fn map_clip_durations(folder: &Path) -> Option<HashMap<u32, f64>> {
    let map_path = find_file(folder, "MAPS.LM0").ok()?;
    Some(clip_durations(&fs::read(map_path).ok()?))
}

fn clip_durations(map_data: &[u8]) -> HashMap<u32, f64> {
    parse_map_records(map_data)
        .into_iter()
        .filter_map(|(key, records)| {
            let record = records.into_iter().next()?;
            let rate = if record.sample_rate == 0 {
                36000
            } else {
                record.sample_rate
            };
            Some((key, f64::from(record.sample_count) / f64::from(rate)))
        })
        .collect()
}

/// Reads a lip-sync file's duration, stored in milliseconds after two header
/// words. `path` is relative to the lip-sync language folder, such as
/// `\S0_0_2Voice\00_15_01.bin`.
fn lip_sync_duration(lip_sync_root: &Path, path: &str) -> Option<f64> {
    let file = path
        .split(['\\', '/'])
        .filter(|part| !part.is_empty())
        .fold(lip_sync_root.to_path_buf(), |file, part| file.join(part));
    let data = fs::read(file).ok()?;
    let milliseconds = u32::from_le_bytes(data.get(8..12)?.try_into().ok()?);
    Some(f64::from(milliseconds) / 1000.0)
}

struct Candidate<'a> {
    name: &'a str,
    export_index: u32,
    key: u32,
    /// The sound's duration according to its lip-sync file, in seconds.
    lip_sync: Option<f64>,
    /// Whether a script plays the sound.
    played: bool,
}

#[derive(Debug, PartialEq)]
enum Change {
    Reversed { first: u32, last: u32, count: usize },
    Swapped { first: String, second: String },
    Moved { name: String, from: u32, to: u32 },
    Paired { name: String, from: u32, to: u32 },
}

impl Change {
    fn describe(&self, package: &str) -> String {
        match self {
            Change::Reversed { first, last, count } => format!(
                "{package}: reversed the stored keys 0x{first:08X}-0x{last:08X} ({count} sounds) to match lip-sync durations."
            ),
            Change::Swapped { first, second } => format!(
                "{package}: swapped the stored keys of {first} and {second} to match lip-sync durations."
            ),
            Change::Moved { name, from, to } => format!(
                "{package}: moved {name} from key 0x{from:08X} to unused key 0x{to:08X} to match its lip-sync duration."
            ),
            Change::Paired { name, from, to } => format!(
                "{package}: moved {name} from key 0x{from:08X}, which has no audio, to key 0x{to:08X}, the package's only audio without a sound."
            ),
        }
    }
}

/// Returns each sound's corrected key, in the order given, and the changes made.
fn correct_package(sounds: &[Candidate], durations: &HashMap<u32, f64>) -> (Vec<u32>, Vec<Change>) {
    let agrees = |key: u32, lip_sync: f64| {
        durations
            .get(&key)
            .is_some_and(|duration| (duration - lip_sync).abs() <= DURATION_TOLERANCE)
    };
    // +1 for each lip-synced sound whose clip has the right length, -1 for each
    // one whose clip doesn't.
    let score = |keys: &[u32], members: &[usize]| -> i32 {
        members
            .iter()
            .filter_map(|&index| {
                let lip_sync = sounds[index].lip_sync?;
                durations
                    .contains_key(&keys[index])
                    .then(|| if agrees(keys[index], lip_sync) { 1 } else { -1 })
            })
            .sum()
    };

    let mut keys = sounds.iter().map(|sound| sound.key).collect::<Vec<_>>();
    let mut changes = Vec::new();

    // Runs of consecutive keys whose names descend may have been stored in
    // reverse. Their ends can belong to neighbouring runs, so every sub-run is
    // tried and the one the lip-sync durations favour most is kept.
    let mut by_key = (0..sounds.len()).collect::<Vec<_>>();
    by_key.sort_by_key(|&index| sounds[index].key);
    let mut start = 0;
    while start < by_key.len() {
        let mut end = start;
        while end + 1 < by_key.len() && sounds[by_key[end + 1]].name < sounds[by_key[end]].name {
            end += 1;
        }
        let mut best: Option<(i32, Vec<usize>, Vec<u32>)> = None;
        for first in start..=end {
            for last in first + 1..=end {
                let run = &by_key[first..=last];
                let low = keys[run[0]];
                let high = keys[run[run.len() - 1]];
                let mut trial = keys.clone();
                for &index in run {
                    trial[index] = low + high - keys[index];
                }
                let gain = score(&trial, run) - score(&keys, run);
                let better = best.as_ref().is_none_or(|(best_gain, best_run, _)| {
                    (gain, run.len()) > (*best_gain, best_run.len())
                });
                if gain > 0 && score(&trial, run) > 0 && better {
                    best = Some((gain, run.to_vec(), trial));
                }
            }
        }
        if let Some((_, run, trial)) = best {
            changes.push(Change::Reversed {
                first: keys[run[0]],
                last: keys[run[run.len() - 1]],
                count: run.len(),
            });
            keys = trial;
        }
        start = end + 1;
    }

    // Neighbouring exports may have each other's keys.
    let mut by_export = (0..sounds.len()).collect::<Vec<_>>();
    by_export.sort_by_key(|&index| sounds[index].export_index);
    for pair in by_export.windows(2) {
        let mut trial = keys.clone();
        trial.swap(pair[0], pair[1]);
        if score(&trial, pair) > score(&keys, pair) && score(&trial, pair) > 0 {
            changes.push(Change::Swapped {
                first: sounds[pair[0]].name.to_owned(),
                second: sounds[pair[1]].name.to_owned(),
            });
            keys = trial;
        }
    }

    // A sound may point next to the key it should use, when no sound owns that key.
    let mut owned = sounds.iter().map(|sound| sound.key).collect::<HashSet<_>>();
    for (index, sound) in sounds.iter().enumerate() {
        let Some(lip_sync) = sound.lip_sync else {
            continue;
        };
        if agrees(keys[index], lip_sync) {
            continue;
        }
        let neighbours = [keys[index].checked_sub(1), keys[index].checked_add(1)];
        if let Some(to) = neighbours
            .into_iter()
            .flatten()
            .find(|key| !owned.contains(key) && agrees(*key, lip_sync))
        {
            changes.push(Change::Moved {
                name: sound.name.to_owned(),
                from: keys[index],
                to,
            });
            owned.insert(to);
            keys[index] = to;
        }
    }

    // A sound added late can point at a key with no audio, while its audio sits
    // under a key no sound owns. With exactly one of each, they belong together.
    let prefixes = keys.iter().map(|key| key >> 16).collect::<HashSet<_>>();
    if let [prefix] = prefixes.into_iter().collect::<Vec<_>>()[..] {
        let current = keys.iter().copied().collect::<HashSet<_>>();
        let unowned = durations
            .keys()
            .filter(|key| *key >> 16 == prefix && !current.contains(key))
            .copied()
            .collect::<Vec<_>>();
        let silent = (0..sounds.len())
            .filter(|&index| sounds[index].played && !durations.contains_key(&keys[index]))
            .collect::<Vec<_>>();
        if let (&[to], &[index]) = (unowned.as_slice(), silent.as_slice()) {
            changes.push(Change::Paired {
                name: sounds[index].name.to_owned(),
                from: keys[index],
                to,
            });
            keys[index] = to;
        }
    }

    (keys, changes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(name: &str, export_index: u32, key: u32, lip_sync: Option<f64>) -> Candidate<'_> {
        Candidate {
            name,
            export_index,
            key,
            lip_sync,
            played: true,
        }
    }

    #[test]
    fn pairs_the_only_played_sound_without_audio_with_the_only_unowned_clip() {
        let durations = HashMap::from([(0x10, 27.7), (0x11, 1.0), (0x12, 2.0)]);
        let mut cut_line = candidate("Play_Cut", 2, 0x40, None);
        cut_line.played = false;
        let sounds = [
            candidate("Play_A", 0, 0x11, None),
            candidate("Play_B", 1, 0x12, None),
            cut_line,
            candidate("Play_Late", 3, 0x41, None),
        ];

        let (keys, changes) = correct_package(&sounds, &durations);

        assert_eq!(keys, [0x11, 0x12, 0x40, 0x10]);
        assert_eq!(
            changes,
            [Change::Paired {
                name: "Play_Late".to_owned(),
                from: 0x41,
                to: 0x10
            }]
        );
    }

    #[test]
    fn leaves_sounds_without_audio_alone_when_the_pairing_is_ambiguous() {
        let durations = HashMap::from([(0x10, 27.7), (0x11, 1.0), (0x12, 2.0)]);
        let sounds = [
            candidate("Play_A", 0, 0x11, None),
            candidate("Play_Late", 1, 0x41, None),
        ];

        let (keys, changes) = correct_package(&sounds, &durations);

        assert_eq!(keys, [0x11, 0x41]);
        assert!(changes.is_empty());
    }

    #[test]
    fn reverses_a_run_whose_clips_match_mirrored_keys() {
        // Keys 10-13 were stored in reverse name order; the clip lengths show
        // A should use 10 and D should use 13.
        let durations = HashMap::from([(9, 5.0), (10, 1.0), (11, 2.0), (12, 3.0), (13, 4.0)]);
        let sounds = [
            candidate("Play_X", 0, 9, Some(5.0)),
            candidate("Play_D", 1, 10, Some(4.0)),
            candidate("Play_C", 2, 11, None),
            candidate("Play_B", 3, 12, Some(2.0)),
            candidate("Play_A", 4, 13, Some(1.0)),
        ];

        let (keys, changes) = correct_package(&sounds, &durations);

        assert_eq!(keys, [9, 13, 12, 11, 10]);
        assert_eq!(
            changes,
            [Change::Reversed {
                first: 10,
                last: 13,
                count: 4
            }]
        );
    }

    #[test]
    fn swaps_the_keys_of_neighbouring_exports() {
        let durations = HashMap::from([(1, 3.0), (2, 8.0), (3, 18.0)]);
        let sounds = [
            candidate("Play_1", 0, 1, Some(3.0)),
            candidate("Play_2", 1, 2, Some(18.0)),
            candidate("Play_3", 2, 3, None),
        ];

        let (keys, changes) = correct_package(&sounds, &durations);

        assert_eq!(keys, [1, 3, 2]);
        assert_eq!(
            changes,
            [Change::Swapped {
                first: "Play_2".to_owned(),
                second: "Play_3".to_owned()
            }]
        );
    }

    #[test]
    fn moves_a_sound_onto_a_neighbouring_key_that_no_sound_owns() {
        let durations = HashMap::from([(5, 1.9), (6, 6.8)]);
        let sounds = [candidate("Play_A", 0, 5, Some(6.8))];

        let (keys, changes) = correct_package(&sounds, &durations);

        assert_eq!(keys, [6]);
        assert_eq!(
            changes,
            [Change::Moved {
                name: "Play_A".to_owned(),
                from: 5,
                to: 6
            }]
        );
    }

    #[test]
    fn keeps_stored_keys_that_the_lip_sync_durations_agree_with() {
        // Descending names alone aren't evidence of a reversed run.
        let durations = HashMap::from([(1, 1.0), (2, 2.0), (3, 3.0)]);
        let sounds = [
            candidate("Play_C", 0, 1, Some(1.0)),
            candidate("Play_B", 1, 2, None),
            candidate("Play_A", 2, 3, Some(3.0)),
        ];

        let (keys, changes) = correct_package(&sounds, &durations);

        assert_eq!(keys, [1, 2, 3]);
        assert!(changes.is_empty());
    }

    #[test]
    fn keeps_stored_keys_without_lip_sync_evidence() {
        let durations = HashMap::from([(1, 1.0), (2, 2.0), (3, 3.0)]);
        let sounds = [
            candidate("Play_C", 0, 1, None),
            candidate("Play_B", 1, 2, None),
            candidate("Play_A", 2, 3, None),
        ];

        let (keys, changes) = correct_package(&sounds, &durations);

        assert_eq!(keys, [1, 2, 3]);
        assert!(changes.is_empty());
    }
}
