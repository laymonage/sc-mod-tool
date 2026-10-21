//! Finding a language's banks: its numeric `.LS0` banks and the sounds in its
//! sound maps, including the extra maps' own.

mod ls0;
mod missions;
mod sound_maps;

pub use sound_maps::SoundKind;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use crate::sounds_folder::{
    LanguageFolder, find_sounds_directory, known_language, map_language_folders, read_directory,
    safe_child_path,
};
use crate::{Bank, Error, GameCache, Language, SegmentSource};
#[cfg(test)]
pub(crate) use ls0::scan_bank as scan_ls0;
use ls0::{collect_banks, read_bank};
use missions::{leading_mission_code, mission_name, mission_names};

pub fn discover_languages(root: &Path) -> Result<Vec<Language>, Error> {
    let sounds = find_sounds_directory(root)?;
    let entries = read_directory(&sounds)?;
    let mut languages = Vec::new();

    for entry in entries {
        let file_type = entry
            .file_type()
            .map_err(|error| format!("Could not inspect {}: {error}", entry.path().display()))?;
        if !file_type.is_dir() {
            continue;
        }

        // Extra maps keep their own sounds in folders beside the languages,
        // such as `1_6_1_1kolacell`, so only known language names count.
        let directory = entry.file_name().to_string_lossy().into_owned();
        let Some(known) = known_language(&directory) else {
            continue;
        };
        let mut banks = Vec::new();
        collect_banks(&entry.path(), Path::new(""), &mut banks)?;
        if !banks.is_empty() {
            languages.push(Language {
                label: known.label.to_owned(),
                directory,
            });
        }
    }

    languages.sort_by(|left, right| {
        left.label
            .to_ascii_lowercase()
            .cmp(&right.label.to_ascii_lowercase())
    });
    if languages.is_empty() {
        return Err(format!(
            "No numeric .LS0 sound banks were found under {}.",
            sounds.display()
        )
        .into());
    }
    Ok(languages)
}

pub fn discover_banks(root: &Path, language: &str) -> Result<Vec<Bank>, Error> {
    static NEVER_CANCELLED: AtomicBool = AtomicBool::new(false);
    discover_banks_with_progress(
        root,
        language,
        &mut GameCache::default(),
        &NEVER_CANCELLED,
        |_, _, _| {},
    )
}

/// Finds `language`'s banks, reading its sound maps' from `cache` when it has
/// them, and adding them when it doesn't.
pub fn discover_banks_with_progress(
    root: &Path,
    language: &str,
    cache: &mut GameCache,
    cancelled: &AtomicBool,
    mut progress: impl FnMut(usize, usize, &str),
) -> Result<Vec<Bank>, Error> {
    let sounds = find_sounds_directory(root)?;
    let mut folders = vec![LanguageFolder {
        map: None,
        relative_path: language.to_owned(),
    }];
    if known_language(language).is_none() || !safe_child_path(&sounds, language)?.is_dir() {
        return Err(format!("Language folder was not found: {language}").into());
    }
    folders.extend(map_language_folders(&sounds, language)?);

    /// A file to read banks from.
    enum Source {
        Bank(PathBuf, PathBuf),
        SoundMap(String),
    }
    let mut sources = Vec::new();
    for (folder_index, folder) in folders.iter().enumerate() {
        let mut folder_files = Vec::new();
        let absolute_path = safe_child_path(&sounds, &folder.relative_path)?;
        collect_banks(
            &absolute_path,
            Path::new(&folder.relative_path),
            &mut folder_files,
        )?;
        sources.extend(
            folder_files
                .into_iter()
                .map(|(absolute, relative)| (folder_index, Source::Bank(absolute, relative))),
        );
        sources.extend(
            sound_maps::map_files(&sounds, folder.map.as_deref(), &folder.relative_path)
                .into_iter()
                .map(|path| (folder_index, Source::SoundMap(path))),
        );
    }

    let mut banks = Vec::new();
    let total = sources.len();
    progress(0, total, "");
    for (index, (folder_index, source)) in sources.into_iter().enumerate() {
        if cancelled.load(Ordering::Relaxed) {
            return Err(Error::Cancelled);
        }
        let map = folders[folder_index].map.as_deref();
        let name = match source {
            Source::Bank(absolute_path, relative_path) => {
                let bank = read_bank(&absolute_path, &relative_path, map, cancelled)?;
                let name = bank.name.clone();
                banks.push((folder_index, 0, bank));
                name
            }
            Source::SoundMap(path) => {
                let map_banks = match cache.map_banks.get(&path) {
                    Some(cached) => cached.to_vec(),
                    None => {
                        let read = sound_maps::read_map_banks(&sounds, &path, map, cancelled)?;
                        cache.map_banks.insert(path.clone(), read.as_slice().into());
                        read
                    }
                };
                for bank in map_banks {
                    let rank = bank.sound_kind.map_or(u8::MAX, |kind| kind as u8 + 1);
                    banks.push((folder_index, rank, bank));
                }
                path
            }
        };
        progress(index + 1, total, &name);
    }
    // Banks are listed by folder, then by mission code. Sound maps' banks
    // have no code, so they come after the folder's `.LS0` banks.
    banks.sort_by_cached_key(|(folder_index, rank, bank)| {
        let code: Option<Vec<u32>> = bank.code.split('_').map(|part| part.parse().ok()).collect();
        (
            *folder_index,
            code.unwrap_or_else(|| vec![u32::MAX]),
            *rank,
            bank.name.to_ascii_lowercase(),
        )
    });

    let names = mission_names(&sounds, language);
    Ok(banks
        .into_iter()
        .map(|(_, _, mut bank)| {
            // Extra maps reuse bank names from other missions, so their
            // mission is the map's, not the bank file's. The map folder name
            // (`1_7_1_1VselkaInfiltration`) starts with its mission code.
            bank.mission_name = match &bank.map {
                Some(map) => {
                    leading_mission_code(map).and_then(|map_code| names.get(&map_code).cloned())
                }
                None => mission_name(&names, &bank.code),
            };
            for segment in &mut bank.segments {
                if let SegmentSource::MapSound { areas, .. } = &mut segment.source {
                    for area in areas {
                        area.mission_name = area
                            .code
                            .as_ref()
                            .and_then(|code| mission_name(&names, code));
                    }
                }
            }
            bank
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decode_segment;
    use crate::tests::{make_segment, make_segment_with_bits};
    use std::fs;

    #[test]
    fn rejects_cancelled_bank_scans() {
        let game = tempfile::tempdir().unwrap();
        let english = game.path().join("Sounds/ENGLISH");
        fs::create_dir_all(&english).unwrap();
        fs::write(english.join("1_1_0.LS0"), make_segment()).unwrap();
        let cancelled = AtomicBool::new(true);

        let error = discover_banks_with_progress(
            game.path(),
            "ENGLISH",
            &mut GameCache::default(),
            &cancelled,
            |_, _, _| {},
        )
        .unwrap_err();
        assert_eq!(error, Error::Cancelled);
    }

    #[test]
    fn reuses_the_sound_maps_banks_it_has_read() {
        let game = tempfile::tempdir().unwrap();
        let sounds = game.path().join("Sounds");
        for language in ["ENGLISH", "GERMAN"] {
            fs::create_dir_all(sounds.join(language)).unwrap();
            fs::write(sounds.join(language).join("1_1_0.LS0"), make_segment()).unwrap();
            fs::write(sounds.join(language).join("MAPS.LM0"), [0; 32]).unwrap();
        }
        // vgmstream can't read these, which gives each map a bank with an
        // issue and the map's size.
        fs::write(sounds.join("MAPS.SM0"), [0; 64]).unwrap();
        let map_sizes = |banks: &[Bank]| {
            banks
                .iter()
                .filter(|bank| bank.path.contains("MAPS"))
                .map(|bank| (bank.path.clone(), bank.size))
                .collect::<Vec<_>>()
        };
        let cancelled = AtomicBool::new(false);
        let mut cache = GameCache::default();
        let scan = |language: &str, cache: &mut GameCache| {
            discover_banks_with_progress(game.path(), language, cache, &cancelled, |_, _, _| {})
                .unwrap()
        };

        scan("ENGLISH", &mut cache);
        fs::write(sounds.join("MAPS.SM0"), [0; 128]).unwrap();
        fs::write(sounds.join("ENGLISH/MAPS.LM0"), [0; 48]).unwrap();
        fs::write(sounds.join("GERMAN/MAPS.LM0"), [0; 48]).unwrap();
        // The shared map comes from the cache, and German's own map is read
        // for the first time.
        assert_eq!(
            map_sizes(&scan("GERMAN", &mut cache)),
            [
                ("GERMAN/MAPS.LM0".to_owned(), 48),
                ("MAPS.SM0".to_owned(), 64),
            ]
        );
        // Going back to English reads its map from the cache too.
        assert_eq!(
            map_sizes(&scan("ENGLISH", &mut cache)),
            [
                ("ENGLISH/MAPS.LM0".to_owned(), 32),
                ("MAPS.SM0".to_owned(), 64),
            ]
        );
        assert_eq!(
            map_sizes(&scan("ENGLISH", &mut GameCache::default())),
            [
                ("ENGLISH/MAPS.LM0".to_owned(), 48),
                ("MAPS.SM0".to_owned(), 128),
            ]
        );
    }

    #[test]
    fn reads_only_ls0_banks_directly_in_the_language_folder() {
        let game = tempfile::tempdir().unwrap();
        let english = game.path().join("Sounds/ENGLISH");
        fs::create_dir_all(english.join("backup")).unwrap();
        for name in ["1_1.LS0", "1_2.ls0", "1_3", "1_4.LS0.bak", "backup/1_1.LS0"] {
            fs::write(english.join(name), make_segment()).unwrap();
        }

        let banks = discover_banks(game.path(), "ENGLISH").unwrap();
        assert_eq!(
            banks
                .iter()
                .map(|bank| bank.path.as_str())
                .collect::<Vec<_>>(),
            ["ENGLISH/1_1.LS0", "ENGLISH/1_2.ls0"]
        );
    }

    #[test]
    fn discovers_languages_and_banks_from_selected_game_directory() {
        let game = tempfile::tempdir().unwrap();
        let english = game.path().join("Sounds/ENGLISH");
        let german = game.path().join("Sounds/GERMAN");
        fs::create_dir_all(&english).unwrap();
        fs::create_dir_all(&german).unwrap();
        fs::write(english.join("1_1_0.LS0"), make_segment()).unwrap();
        fs::write(english.join("2_2.LS0"), [0; 200]).unwrap();
        fs::write(english.join("Music_Common.LS0"), make_segment()).unwrap();
        fs::write(german.join("1_1_0.LS0"), make_segment()).unwrap();
        fs::write(game.path().join("Sounds/1_1_0.LS0"), make_segment()).unwrap();

        let languages = discover_languages(game.path()).unwrap();
        assert_eq!(
            languages
                .iter()
                .map(|language| language.directory.as_str())
                .collect::<Vec<_>>(),
            ["ENGLISH", "GERMAN"]
        );
        assert!(discover_banks(game.path(), ".").is_err());
        let banks = discover_banks(game.path(), "ENGLISH").unwrap();
        assert_eq!(banks.len(), 2);
        let supported = banks.iter().find(|bank| bank.name == "1_1_0.LS0").unwrap();
        assert_eq!(supported.path, "ENGLISH/1_1_0.LS0");
        assert_eq!(supported.issue, None);
        assert_eq!(supported.segments.len(), 1);
        assert_eq!(supported.segments[0].stream_range(), Some((0, 103)));
        let unsupported = banks.iter().find(|bank| bank.name == "2_2.LS0").unwrap();
        assert!(unsupported.segments.is_empty());
        assert!(
            unsupported
                .issue
                .as_deref()
                .unwrap()
                .contains("unsupported layout")
        );
    }

    #[test]
    fn finds_extra_map_banks_under_their_language_instead_of_as_languages() {
        let game = tempfile::tempdir().unwrap();
        let italian = game.path().join("Sounds/ITALIEN");
        let extra_map = game.path().join("Sounds/1_6_1_1kolacell");
        fs::create_dir_all(&italian).unwrap();
        fs::create_dir_all(extra_map.join("English")).unwrap();
        fs::create_dir_all(extra_map.join("Italien")).unwrap();
        fs::write(italian.join("1_1_0.LS0"), make_segment()).unwrap();
        fs::write(extra_map.join("English/3_4_2.LS0"), make_segment()).unwrap();
        // Extra maps' banks use 6-bit codes.
        fs::write(
            extra_map.join("Italien/3_4_2.LS0"),
            make_segment_with_bits(6),
        )
        .unwrap();

        let languages = discover_languages(game.path()).unwrap();
        assert_eq!(
            languages
                .iter()
                .map(|language| (language.directory.as_str(), language.label.as_str()))
                .collect::<Vec<_>>(),
            [("ITALIEN", "Italian")]
        );

        let banks = discover_banks(game.path(), "ITALIEN").unwrap();
        assert_eq!(
            banks
                .iter()
                .map(|bank| (bank.path.as_str(), bank.map.as_deref()))
                .collect::<Vec<_>>(),
            [
                ("ITALIEN/1_1_0.LS0", None),
                ("1_6_1_1kolacell/Italien/3_4_2.LS0", Some("1_6_1_1kolacell")),
            ]
        );
        assert_eq!(
            banks[1].segments[0].id,
            "1_6_1_1kolacell/Italien/3_4_2.LS0:0"
        );
        assert_eq!(banks[1].segments[0].codec, "Ubisoft ADPCM 6-bit");
        let cancelled = AtomicBool::new(false);
        let decoded = decode_segment(
            game.path(),
            &banks[1].path,
            &banks[1].segments[0],
            &cancelled,
        )
        .unwrap();
        assert_eq!(decoded.samples.len(), 4);
    }
}
