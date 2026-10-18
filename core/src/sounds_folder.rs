//! Finding the game's `Sounds` folder, the language folders in it, and files
//! in those, whose names the game doesn't always capitalize consistently.

use std::fs;
use std::path::{Component, Path, PathBuf};

pub(crate) fn find_sounds_directory(root: &Path) -> Result<PathBuf, String> {
    let root = root
        .canonicalize()
        .map_err(|error| format!("Could not open selected folder {}: {error}", root.display()))?;
    if root
        .file_name()
        .is_some_and(|name| name.to_string_lossy().eq_ignore_ascii_case("sounds"))
    {
        return Ok(root);
    }

    for name in ["Sounds", "sounds"] {
        let candidate = root.join(name);
        if candidate.is_dir() {
            return candidate.canonicalize().map_err(|error| {
                format!(
                    "Could not open sound folder {}: {error}",
                    candidate.display()
                )
            });
        }
    }
    Err(format!(
        "No Sounds directory was found in {}. Select the game installation or its Sounds folder.",
        root.display()
    ))
}

pub(crate) fn safe_child_path(root: &Path, relative: &str) -> Result<PathBuf, String> {
    let relative_path = Path::new(relative);
    if relative_path.as_os_str().is_empty()
        || relative_path
            .components()
            .any(|component| !matches!(component, Component::Normal(_) | Component::CurDir))
    {
        return Err("The requested sound-bank path is invalid.".to_owned());
    }
    let candidate = root.join(relative_path);
    let canonical = candidate
        .canonicalize()
        .map_err(|error| format!("Could not open {}: {error}", candidate.display()))?;
    if !canonical.starts_with(root) {
        return Err("The requested path is outside the selected Sounds directory.".to_owned());
    }
    Ok(canonical)
}

pub(crate) fn read_directory(path: &Path) -> Result<Vec<fs::DirEntry>, String> {
    fs::read_dir(path)
        .map_err(|error| format!("Could not read directory {}: {error}", path.display()))?
        .map(|entry| {
            entry.map_err(|error| format!("Could not read an entry in {}: {error}", path.display()))
        })
        .collect()
}

pub(crate) fn find_file(directory: &Path, file_name: &str) -> Result<PathBuf, String> {
    let entries = fs::read_dir(directory)
        .map_err(|error| format!("Could not read directory {}: {error}", directory.display()))?;
    for entry in entries {
        let entry =
            entry.map_err(|error| format!("Could not inspect {}: {error}", directory.display()))?;
        if entry
            .file_name()
            .to_string_lossy()
            .eq_ignore_ascii_case(file_name)
        {
            return Ok(entry.path());
        }
    }
    Err(format!(
        "File {file_name} was not found in {}.",
        directory.display()
    ))
}

/// A folder of one language's banks, relative to `Sounds`.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct LanguageFolder {
    /// The extra map the folder belongs to, or `None` for the language's own
    /// folder.
    pub(crate) map: Option<String>,
    pub(crate) relative_path: String,
}

/// Finds `language`'s folders in the extra maps beside the languages under
/// `Sounds`, such as `1_6_1_1kolacell/English`, sorted by map name. Each map
/// has its own banks and `MAPS.LM0` for each language it ships.
pub(crate) fn map_language_folders(
    sounds: &Path,
    language: &str,
) -> Result<Vec<LanguageFolder>, String> {
    let Some(language) = known_language(language) else {
        return Ok(Vec::new());
    };
    let mut folders = Vec::new();
    for entry in read_directory(sounds)? {
        let map = entry.file_name().to_string_lossy().into_owned();
        if !entry.path().is_dir() || known_language(&map).is_some() {
            continue;
        }
        for child in read_directory(&entry.path())? {
            let child_name = child.file_name().to_string_lossy().into_owned();
            if child.path().is_dir() && known_language(&child_name).as_ref() == Some(&language) {
                folders.push(LanguageFolder {
                    relative_path: format!("{map}/{child_name}"),
                    map: Some(map.clone()),
                });
            }
        }
    }
    folders.sort_by(|left, right| {
        left.relative_path
            .to_ascii_lowercase()
            .cmp(&right.relative_path.to_ascii_lowercase())
    });
    Ok(folders)
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct KnownLanguage {
    pub(crate) label: &'static str,
    /// The extension of the game's localization files, such as `Echelon.int`.
    pub(crate) localization_extension: &'static str,
}

/// Looks up a language by its folder under `Sounds`, ignoring case.
pub(crate) fn known_language(directory: &str) -> Option<KnownLanguage> {
    let (label, localization_extension) = match directory.to_ascii_uppercase().as_str() {
        "ENGLISH" => ("English", "int"),
        "FRENCH" => ("French", "fra"),
        "GERMAN" => ("German", "deu"),
        // The game names this folder in French.
        "ITALIEN" | "ITALIAN" => ("Italian", "ita"),
        "SPANISH" => ("Spanish", "esp"),
        _ => return None,
    };
    Some(KnownLanguage {
        label,
        localization_extension,
    })
}
