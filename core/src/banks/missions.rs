//! Mission codes, such as `1_1_0`, and the mission names the game's `HUD`
//! localization gives them.

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::Path;

use encoding_rs::WINDOWS_1252;

use crate::sounds_folder::known_language;

pub(super) fn mission_code(file_name: &str) -> Option<String> {
    let stem = file_name
        .rsplit_once('.')
        .filter(|(_, extension)| extension.eq_ignore_ascii_case("LS0"))
        .map_or(file_name, |(stem, _)| stem);
    let valid = stem
        .split('_')
        .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()));
    if valid && stem.contains('_') {
        Some(stem.to_owned())
    } else {
        None
    }
}

/// The display name of the mission with `code`. Shared banks (`1_1`), whose
/// lines several levels play, aren't named in `HUD`, so they take their
/// mission's first level's name (`1_1_0`).
pub(super) fn mission_name(names: &BTreeMap<String, String>, code: &str) -> Option<String> {
    if let Some(name) = names.get(code) {
        return Some(name.clone());
    }
    if code.matches('_').count() != 1 {
        return None;
    }
    let prefix = format!("{code}_");
    names
        .range(prefix.clone()..)
        .next()
        .filter(|(level, _)| level.starts_with(&prefix))
        .map(|(_, name)| name.clone())
}

/// Reads the mission display names from the game's `HUD` localization file in
/// `language`, or the English one when that's missing, keyed by mission code
/// such as `1_1_0`. Empty when neither file can be read.
pub(super) fn mission_names(sounds: &Path, language: &str) -> BTreeMap<String, String> {
    let Some(game_root) = sounds.parent() else {
        return BTreeMap::new();
    };
    let localization_root = game_root.join("system").join("LOCALIZATION");
    let extension = known_language(language).map_or("int", |known| known.localization_extension);
    let Some(bytes) = [extension, "int"].into_iter().find_map(|extension| {
        crate::subtitles::localization::localization_path(&localization_root, "HUD", extension)
            .ok()
            .and_then(|path| fs::read(path).ok())
    }) else {
        return BTreeMap::new();
    };
    let (text, _, _) = WINDOWS_1252.decode(&bytes);
    let mut sections = HashMap::new();
    crate::subtitles::localization::parse_sections(&text, &mut sections);
    sections
        .remove("hud")
        .into_iter()
        .flatten()
        .filter_map(|(key, name)| Some((leading_mission_code(&key)?, name)))
        .collect()
}

/// The mission code a `HUD` key or extra map folder starts with, such as
/// `1_1_0` from `1_1_0Tbilisi` or `0_0_2` from `0_0_2_Training`.
pub(super) fn leading_mission_code(name: &str) -> Option<String> {
    let end = name
        .find(|c: char| !c.is_ascii_digit() && c != '_')
        .unwrap_or(name.len());
    mission_code(name[..end].trim_end_matches('_'))
}

#[cfg(test)]
mod tests {
    use crate::discover_banks;
    use crate::tests::make_segment;
    use std::fs;

    #[test]
    fn reads_mission_names_from_the_games_hud_localization() {
        let game = tempfile::tempdir().unwrap();
        let english = game.path().join("Sounds/ENGLISH");
        fs::create_dir_all(&english).unwrap();
        for bank in ["1_1_0.LS0", "1_1.LS0", "4_2.LS0", "3_1_1.LS0"] {
            fs::write(english.join(bank), make_segment()).unwrap();
        }
        // An extra map reuses a bank name from another mission.
        let vselka = game.path().join("Sounds/1_7_1_2Vselka/English");
        fs::create_dir_all(&vselka).unwrap();
        fs::write(vselka.join("3_1_1.LS0"), make_segment()).unwrap();
        let localization = game.path().join("system/LOCALIZATION");
        fs::create_dir_all(&localization).unwrap();
        fs::write(
            localization.join("HUD.int"),
            "[HUD]\n1_1_0Tbilisi=Police Station\n1_1_1Tbilisi=Police Station\n4_2_1_Abattoir=Abattoir\n1_7_1_2Vselka=Vselka Submarine\n",
        )
        .unwrap();

        let banks = discover_banks(game.path(), "ENGLISH").unwrap();
        let name = |file_name: &str| {
            banks
                .iter()
                .find(|bank| bank.name == file_name)
                .unwrap()
                .mission_name
                .clone()
        };
        // A level bank uses its own entry.
        assert_eq!(name("1_1_0.LS0").as_deref(), Some("Police Station"));
        // A shared bank uses its mission's first level's name.
        assert_eq!(name("1_1.LS0").as_deref(), Some("Police Station"));
        assert_eq!(name("4_2.LS0").as_deref(), Some("Abattoir"));
        // A level bank with no HUD entry has no name.
        assert_eq!(name("3_1_1.LS0"), None);
        // An extra map's bank takes its mission from the map, not the reused
        // bank name.
        let vselka_bank = banks
            .iter()
            .find(|bank| bank.map.as_deref() == Some("1_7_1_2Vselka"))
            .unwrap();
        assert_eq!(
            vselka_bank.mission_name.as_deref(),
            Some("Vselka Submarine")
        );
    }

    #[test]
    fn reads_mission_names_in_the_banks_language() {
        let game = tempfile::tempdir().unwrap();
        for language in ["FRENCH", "GERMAN"] {
            let folder = game.path().join("Sounds").join(language);
            fs::create_dir_all(&folder).unwrap();
            fs::write(folder.join("1_1_0.LS0"), make_segment()).unwrap();
        }
        let localization = game.path().join("system/LOCALIZATION");
        fs::create_dir_all(&localization).unwrap();
        fs::write(
            localization.join("HUD.int"),
            "[HUD]\n1_1_0Tbilisi=Police Station\n",
        )
        .unwrap();
        fs::write(
            localization.join("HUD.fra"),
            b"[HUD]\r\n1_1_0Tbilisi=Poste de Police\r\n0_0_2_Training=Entra\xeenement\r\n",
        )
        .unwrap();

        let name = |language: &str| {
            discover_banks(game.path(), language).unwrap()[0]
                .mission_name
                .clone()
        };
        assert_eq!(name("FRENCH").as_deref(), Some("Poste de Police"));
        // A language without its own `HUD` file falls back to English.
        assert_eq!(name("GERMAN").as_deref(), Some("Police Station"));
    }
}
