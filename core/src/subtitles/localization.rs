use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use encoding_rs::WINDOWS_1252;

use crate::sounds_folder::find_file;

/// A localization line's text, or `None` for a line without text: the game's
/// files use `(null)` for those.
pub(super) fn line_text(text: &str) -> Option<String> {
    let text = text.trim();
    (!text.is_empty() && !text.eq_ignore_ascii_case("(null)")).then(|| text.to_owned())
}

/// Reads every section of the localization files with `extension`, keeping
/// each section's lines in file order. Cue names can repeat within a section.
pub(super) fn localization_sections(
    localization_root: &Path,
    extension: &str,
) -> Result<HashMap<String, Vec<(String, String)>>, String> {
    let entries = fs::read_dir(localization_root).map_err(|error| {
        format!(
            "Could not read directory {}: {error}",
            localization_root.display()
        )
    })?;
    let mut paths = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .is_some_and(|value| value.eq_ignore_ascii_case(extension))
        })
        .collect::<Vec<_>>();
    paths.sort();
    let mut sections = HashMap::<String, Vec<(String, String)>>::new();
    for path in paths {
        let Ok(bytes) = fs::read(&path) else {
            continue;
        };
        let (text, _, _) = WINDOWS_1252.decode(&bytes);
        parse_sections(&text, &mut sections);
    }
    Ok(sections)
}

pub(crate) fn parse_sections(text: &str, sections: &mut HashMap<String, Vec<(String, String)>>) {
    let mut current: Option<String> = None;
    for line in text.lines().map(str::trim) {
        if let Some(name) = line
            .strip_prefix('[')
            .and_then(|line| line.strip_suffix(']'))
        {
            let name = name.trim().to_ascii_lowercase();
            // A section that appears in several files keeps its first copy.
            current = (!sections.contains_key(&name)).then(|| {
                sections.insert(name.clone(), Vec::new());
                name
            });
            continue;
        }
        if let (Some(section), Some((key, value))) = (&current, line.split_once('=')) {
            sections
                .get_mut(section)
                .expect("current section was inserted")
                .push((key.trim().to_owned(), value.trim().to_owned()));
        }
    }
}

pub(crate) fn localization_path(
    localization_root: &Path,
    base_name: &str,
    extension: &str,
) -> Result<PathBuf, String> {
    let file_name = format!("{base_name}.{extension}");
    let exact_path = localization_root.join(&file_name);
    if exact_path.is_file() {
        return Ok(exact_path);
    }

    find_file(localization_root, &file_name).map_err(|_| {
        format!(
            "Localization file {file_name} was not found in {}.",
            localization_root.display()
        )
    })
}

pub(super) fn find_cue(text: &str, section: &str, cue: &str) -> Result<String, String> {
    let mut active_section = "";
    let mut found = None;

    for line in text.lines().map(str::trim) {
        if let Some(section_name) = line
            .strip_prefix('[')
            .and_then(|line| line.strip_suffix(']'))
        {
            active_section = section_name.trim();
            continue;
        }
        if active_section.eq_ignore_ascii_case(section)
            && let Some((key, value)) = line.split_once('=')
            && key.trim().eq_ignore_ascii_case(cue)
        {
            if found.is_some() {
                return Err(format!(
                    "Localization cue {cue} occurs more than once in section {section}."
                ));
            }
            found = Some(value.trim().to_owned());
        }
    }

    found.ok_or_else(|| format!("Localization cue {cue} was not found in section {section}."))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_cue_in_the_requested_localization_section() {
        let text = "[Other]\nSpeech_0001L=Wrong\n[Pilot]\nSpeech_0001L=Speaker - Custom line\n";

        assert_eq!(
            find_cue(text, "Pilot", "Speech_0001L").unwrap(),
            "Speaker - Custom line"
        );
    }

    #[test]
    fn rejects_duplicate_or_missing_cues() {
        let duplicate = "[Pilot]\nSpeech_0001L=First\nSpeech_0001L=Second\n";
        assert!(find_cue(duplicate, "Pilot", "Speech_0001L").is_err());
        assert!(find_cue("[Pilot]\n", "Pilot", "Speech_0001L").is_err());
    }

    #[test]
    fn keeps_empty_cues_as_lines_without_text() {
        let text = find_cue("[Pilot]\nSpeech_0001L=\n", "Pilot", "Speech_0001L").unwrap();
        assert_eq!(line_text(&text), None);
    }

    #[test]
    fn keeps_section_lines_in_file_order_with_repeated_cues() {
        let mut sections = HashMap::new();
        parse_sections(
            "[Talk]\nSpeech_0020L=First\nSpeech_0001L=Second\nSpeech_0028L=Third\nSpeech_0028L=Fourth\n[Other]\nA=B\n",
            &mut sections,
        );

        assert_eq!(
            sections["talk"],
            [
                ("Speech_0020L".to_owned(), "First".to_owned()),
                ("Speech_0001L".to_owned(), "Second".to_owned()),
                ("Speech_0028L".to_owned(), "Third".to_owned()),
                ("Speech_0028L".to_owned(), "Fourth".to_owned()),
            ]
        );
    }
}
