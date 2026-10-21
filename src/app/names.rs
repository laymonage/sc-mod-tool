//! What banks and segments are called, in the UI and as exported files.

use std::collections::HashSet;

use sc_mod_tool_core::{Bank, Segment};

pub(super) fn segment_label(bank: &Bank, segment: &Segment) -> String {
    format!("{} #{}", bank_title(bank), segment.segment + 1)
}

/// The file name an exported segment is offered: a map sound's name, such as
/// `ROGEAR_2.wav`, or else its bank's file and its number, such as
/// `1_1_1-12.wav`.
pub(super) fn export_file_name(bank: &Bank, segment: &Segment) -> String {
    format!("{}.wav", export_file_stem(bank, segment))
}

/// The file names a bank's segments are exported as, in its order. Map sounds
/// can share a name, so later ones are numbered, such as `Music (2).wav`.
/// Names are compared ignoring case, as macOS and Windows do.
pub(super) fn export_file_names(bank: &Bank) -> Vec<String> {
    let mut taken = HashSet::new();
    bank.segments
        .iter()
        .map(|segment| {
            let stem = export_file_stem(bank, segment);
            let mut name = stem.clone();
            let mut copy = 1;
            while !taken.insert(name.to_lowercase()) {
                copy += 1;
                name = format!("{stem} ({copy})");
            }
            format!("{name}.wav")
        })
        .collect()
}

fn export_file_stem(bank: &Bank, segment: &Segment) -> String {
    let name = segment
        .name
        .clone()
        .unwrap_or_else(|| format!("{}-{}", bank_file_stem(bank), segment.segment + 1));
    file_name_safe(&name)
}

/// The folder a bank is exported to: its file's name, such as `1_1_1`, or a
/// sound map's name and the kind of sounds, such as `MAPS.SM0 - Music`, after
/// its extra map's folder when it has one.
pub(super) fn export_folder_name(bank: &Bank) -> String {
    let file = bank_file_name(bank);
    let mut parts: Vec<&str> = bank.map.iter().map(String::as_str).collect();
    match bank.sound_kind {
        // A language's sound map has the same name as the shared one, with
        // another extension.
        Some(_) => parts.extend([file, bank.name.as_str()]),
        None => parts.push(bank_file_stem(bank)),
    }
    file_name_safe(&parts.join(" - "))
}

fn bank_file_name(bank: &Bank) -> &str {
    bank.path.rsplit('/').next().unwrap_or(&bank.path)
}

fn bank_file_stem(bank: &Bank) -> &str {
    let file = bank_file_name(bank);
    file.rsplit_once('.').map_or(file, |(stem, _)| stem)
}

/// Replaces characters some systems don't allow in file names, which map
/// sounds' names, being descriptions, may hold.
fn file_name_safe(name: &str) -> String {
    name.chars()
        .map(|c| if r#"/\:*?"<>|"#.contains(c) { '_' } else { c })
        .collect()
}

/// The bank's name, after its extra map's when it has one, since maps reuse
/// bank names.
pub(super) fn bank_title(bank: &Bank) -> String {
    match &bank.map {
        Some(map) => format!("{map} / {}", bank.name),
        None => bank.name.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::test_support::{bank, segment};
    use sc_mod_tool_core::SoundKind;

    #[test]
    fn names_export_folders_after_banks() {
        assert_eq!(
            export_folder_name(&bank("ENGLISH/1_1_1.LS0", None, None)),
            "1_1_1"
        );
        assert_eq!(
            export_folder_name(&bank("ENGLISH/MAPS.LM0", None, Some(SoundKind::Music))),
            "MAPS.LM0 - Music"
        );
        assert_eq!(
            export_folder_name(&bank(
                "1_6_1_1kolacell/MAPS.SM0",
                Some("1_6_1_1kolacell"),
                Some(SoundKind::Music)
            )),
            "1_6_1_1kolacell - MAPS.SM0 - Music"
        );
    }

    #[test]
    fn numbers_exported_sounds_that_share_a_name() {
        let mut bank = bank("MAPS.SM0", None, Some(SoundKind::Music));
        let named = |name: &str| Segment {
            name: Some(name.to_owned()),
            ..segment(1, 1)
        };
        bank.segments = vec![
            named("Sequence (multi-loop)"),
            named("ROGEAR_2"),
            named("sequence (multi-loop)"),
            named("Sequence (multi-loop)"),
            named("a/b"),
        ];
        assert_eq!(
            export_file_names(&bank),
            [
                "Sequence (multi-loop).wav",
                "ROGEAR_2.wav",
                "sequence (multi-loop) (2).wav",
                "Sequence (multi-loop) (3).wav",
                "a_b.wav",
            ]
        );
    }

    #[test]
    fn names_exported_stream_segments_by_bank_and_number() {
        let mut bank = bank("ENGLISH/1_1_1.LS0", None, None);
        bank.segments = vec![
            segment(1, 1),
            Segment {
                segment: 1,
                ..segment(1, 1)
            },
        ];
        assert_eq!(export_file_names(&bank), ["1_1_1-1.wav", "1_1_1-2.wav"]);
    }
}
