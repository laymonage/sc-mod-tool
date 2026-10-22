use std::collections::HashMap;

use super::bridge::{BridgeCall, BridgePattern};
use super::localization::line_text;
use super::map::{SoundMap, clip_seconds, resolve_segments};
use super::{
    MappingConfidence, Subtitle, SubtitleAnalysis, UnshownLine, add_subtitle, push_warning,
};

/// Matches the sounds a pattern plays without a subtitle to the lines of the
/// localization section named after the pattern, by position.
///
/// A pattern's lines of dialogue and its section's spoken lines are in the
/// same order. Other lines, such as `Note_` and `Goal_` text shown on screen,
/// aren't voiced and are skipped. The alignment is made against the English
/// section (`reference_sections`), and each line's text is then looked up in
/// this language by cue name: other languages' sections can keep lines that
/// English dropped, which would throw positions off. If English doesn't line
/// up, this language's section is aligned on its own.
pub(super) fn match_unshown_subtitles(
    analysis: &mut SubtitleAnalysis,
    pattern: &BridgePattern,
    sections: &HashMap<String, Vec<(String, String)>>,
    reference_sections: &HashMap<String, Vec<(String, String)>>,
    maps: &[SoundMap],
) {
    let lines = spoken_lines(sections, &pattern.pattern);
    let reference_lines = spoken_lines(reference_sections, &pattern.pattern);
    let dialogue = pattern
        .calls
        .iter()
        .filter(|call| call.is_dialogue())
        .collect::<Vec<_>>();
    // Only scenes that speak lines without a subtitle are of interest.
    if lines.is_empty() || dialogue.iter().all(|call| call.cue.is_some()) {
        return;
    }
    // How many earlier dialogue calls in the same source object play the same
    // sound, counting every call, as the scripts read.
    let mut counts = HashMap::<(&str, &str), usize>::new();
    let occurrences = dialogue
        .iter()
        .map(|call| {
            let count = counts
                .entry((&call.source_object, &call.sound_event))
                .or_default();
            *count += 1;
            *count - 1
        })
        .collect::<Vec<_>>();
    let align_with = |section_lines: &[&(String, String)]| {
        let speaking = section_calls(&pattern.pattern, &dialogue, section_lines);
        let calls = speaking
            .iter()
            .map(|&index| dialogue[index])
            .collect::<Vec<_>>();
        let durations = calls
            .iter()
            .map(|call| call.sound_map_key.and_then(|key| clip_seconds(key, maps)))
            .collect::<Vec<_>>();
        align(&pattern.pattern, &calls, section_lines, &durations)
            .map(|alignment| (speaking, alignment))
    };

    let (speaking, matched) = match align_with(&reference_lines) {
        Ok((speaking, alignment)) => (
            speaking,
            alignment
                .iter()
                .map(|&line| cue_in_language(&reference_lines, line, &lines))
                .collect::<Vec<_>>(),
        ),
        Err(_) => match align_with(&lines) {
            Ok((speaking, alignment)) => (
                speaking,
                alignment.iter().map(|&line| Some(lines[line])).collect(),
            ),
            Err(warning) => {
                push_warning(&mut analysis.warnings, warning);
                return;
            }
        },
    };

    for (index, line) in speaking.into_iter().zip(matched) {
        let call = dialogue[index];
        let Some((cue, text)) = line else {
            continue;
        };
        if call.cue.is_some() {
            continue;
        }
        let mut unshown = UnshownLine {
            pattern: pattern.pattern.clone(),
            source_object: call.source_object.clone(),
            sound_event: call.sound_event.clone(),
            function: call.function.clone(),
            occurrence: occurrences[index],
            cue: cue.clone(),
            text: line_text(text),
            segments: Vec::new(),
        };
        let Some(key) = call.sound_map_key else {
            analysis.unshown_lines.push(unshown);
            continue;
        };
        let segments = match resolve_segments(&call.sound_event, key, maps) {
            Ok(segments) => segments,
            Err(warning) => {
                push_warning(&mut analysis.warnings, warning);
                analysis.unshown_lines.push(unshown);
                continue;
            }
        };
        unshown.segments = segments.iter().map(|segment| segment.id.clone()).collect();
        analysis.unshown_lines.push(unshown);
        for segment in segments {
            add_subtitle(
                analysis,
                &segment.id,
                Subtitle {
                    cue: cue.clone(),
                    text: line_text(text),
                    sources: vec![format!(
                        "{}: {}",
                        pattern.source_package, call.source_object
                    )],
                    sound_event: call.sound_event.clone(),
                    sound_map_key: key,
                    confidence: MappingConfidence::SectionOrder,
                },
            );
        }
        analysis.unshown_pairs += 1;
    }
}

/// A section's spoken lines, in file order. `Note_` and `Goal_` lines are text
/// shown on screen and aren't voiced.
fn spoken_lines<'a>(
    sections: &'a HashMap<String, Vec<(String, String)>>,
    section: &str,
) -> Vec<&'a (String, String)> {
    sections
        .get(&section.to_ascii_lowercase())
        .map(|lines| {
            lines
                .iter()
                .filter(|(key, _)| key.to_ascii_lowercase().starts_with("speech_"))
                .collect()
        })
        .unwrap_or_default()
}

/// The dialogue calls, by index, that speak a line of the scene's own section.
///
/// A radio message that names another scene's cue, or plays a cue more often
/// than the section has lines for it, speaks a line counted elsewhere, so it
/// doesn't take a position. EnhancedSC adds both: a game over that reuses
/// another scene's warning, and a second branch that repeats a warning.
fn section_calls(pattern: &str, calls: &[&BridgeCall], lines: &[&(String, String)]) -> Vec<usize> {
    let mut played = HashMap::<String, usize>::new();
    (0..calls.len())
        .filter(|&index| match (&calls[index].section, &calls[index].cue) {
            (Some(section), Some(cue)) => {
                if !section.eq_ignore_ascii_case(pattern) {
                    return false;
                }
                let available = lines
                    .iter()
                    .filter(|(key, _)| key.eq_ignore_ascii_case(cue))
                    .count();
                let count = played.entry(cue.to_ascii_lowercase()).or_default();
                *count += 1;
                // A cue the section lacks keeps its position, so the
                // alignment fails its anchor check instead of hiding it.
                available == 0 || *count <= available
            }
            _ => true,
        })
        .collect()
}

/// Lines a scene's dialogue calls up with a section's spoken lines, returning
/// each call's line, or a warning explaining why they don't line up.
fn align(
    pattern: &str,
    calls: &[&BridgeCall],
    lines: &[&(String, String)],
    durations: &[Option<f64>],
) -> Result<Vec<usize>, String> {
    if lines.is_empty() {
        return Err(format!(
            "{pattern} has no spoken lines in its localization section, so its unshown subtitles weren't matched."
        ));
    }
    if lines.len() == calls.len() {
        let alignment = (0..calls.len()).collect::<Vec<_>>();
        return if is_anchored(pattern, calls, lines, &alignment) {
            Ok(alignment)
        } else {
            Err(format!(
                "{pattern}'s subtitled sounds don't line up with its localization section, so its unshown subtitles weren't matched."
            ))
        };
    }
    if lines.len() > calls.len() {
        return skipping_alignment(pattern, calls, lines, durations).ok_or_else(|| {
            format!(
                "{pattern} speaks {} lines but its localization section has {}, and their lengths don't show which lines it skips, so its unshown subtitles weren't matched.",
                calls.len(),
                lines.len()
            )
        });
    }
    Err(format!(
        "{pattern} speaks {} lines but its localization section has only {}, so its unshown subtitles weren't matched.",
        calls.len(),
        lines.len()
    ))
}

/// Finds the line of `lines` with the same cue as `reference_lines[index]`.
/// Cue names can repeat within a section, so the same occurrence is used.
fn cue_in_language<'a>(
    reference_lines: &[&(String, String)],
    index: usize,
    lines: &[&'a (String, String)],
) -> Option<&'a (String, String)> {
    let cue = &reference_lines[index].0;
    let occurrence = reference_lines[..index]
        .iter()
        .filter(|(key, _)| key.eq_ignore_ascii_case(cue))
        .count();
    lines
        .iter()
        .filter(|(key, _)| key.eq_ignore_ascii_case(cue))
        .nth(occurrence)
        .copied()
}

/// Whether every call with a `Localize` cue sits at that cue's line, where
/// `alignment` gives each call's line.
fn is_anchored(
    pattern: &str,
    calls: &[&BridgeCall],
    lines: &[&(String, String)],
    alignment: &[usize],
) -> bool {
    calls
        .iter()
        .zip(alignment)
        .all(|(call, &line)| match (&call.section, &call.cue) {
            (Some(section), Some(cue)) => {
                section.eq_ignore_ascii_case(pattern) && cue.eq_ignore_ascii_case(&lines[line].0)
            }
            _ => true,
        })
}

/// The most ways of skipping a section's extra lines that are compared.
const MAX_SKIP_ALIGNMENTS: usize = 200;

/// When a section has lines its scene never speaks, finds which ones to skip.
///
/// Each way of skipping the extra lines is scored by how much the speaking
/// rate (characters per second of clip) varies across the lines it keeps. The
/// most even alignment is used only when it clearly beats every other.
fn skipping_alignment(
    pattern: &str,
    calls: &[&BridgeCall],
    lines: &[&(String, String)],
    durations: &[Option<f64>],
) -> Option<Vec<usize>> {
    let mut scored = Vec::new();
    for skipped in combinations(lines.len(), lines.len() - calls.len(), MAX_SKIP_ALIGNMENTS)? {
        let alignment = (0..lines.len())
            .filter(|line| !skipped.contains(line))
            .collect::<Vec<_>>();
        if !is_anchored(pattern, calls, lines, &alignment) {
            continue;
        }
        let log_rates = alignment
            .iter()
            .zip(durations)
            .filter_map(|(&line, duration)| {
                Some((spoken_length(&lines[line].1) as f64 / (*duration)?).ln())
            })
            .collect::<Vec<_>>();
        if log_rates.len() < 2 {
            continue;
        }
        let mean = log_rates.iter().sum::<f64>() / log_rates.len() as f64;
        let spread = log_rates
            .iter()
            .map(|rate| (rate - mean).powi(2))
            .sum::<f64>();
        scored.push((spread, alignment));
    }
    scored.sort_by(|left, right| left.0.total_cmp(&right.0));
    match scored.as_slice() {
        [(_, best)] => Some(best.clone()),
        [(best, alignment), (second, _), ..] => (best * 2.0 < *second).then(|| alignment.clone()),
        [] => None,
    }
}

/// Every way of choosing `count` of `0..total`, or `None` if there are more
/// than `limit`.
fn combinations(total: usize, count: usize, limit: usize) -> Option<Vec<Vec<usize>>> {
    let mut size = 1usize;
    for index in 0..count.min(total - count) {
        size = size.checked_mul(total - index)? / (index + 1);
        if size > limit {
            return None;
        }
    }
    let mut all = Vec::with_capacity(size);
    let mut current = (0..count).collect::<Vec<_>>();
    loop {
        all.push(current.clone());
        let Some(position) = (0..count)
            .rev()
            .find(|&position| current[position] < total - count + position)
        else {
            return Some(all);
        };
        current[position] += 1;
        for next in position + 1..count {
            current[next] = current[next - 1] + 1;
        }
    }
}

/// Counts the characters spoken in a line, leaving out a leading speaker name
/// such as `SOLDIER A - ` and stage directions in parentheses.
fn spoken_length(text: &str) -> usize {
    let text = match text.split_once(" - ") {
        Some((speaker, rest))
            if !speaker.trim().is_empty()
                && speaker.trim().chars().all(|character| {
                    character.is_ascii_uppercase() || character.is_ascii_digit() || character == ' '
                }) =>
        {
            rest
        }
        _ => text,
    };
    let mut depth = 0usize;
    text.chars()
        .filter(|&character| {
            match character {
                '(' => depth += 1,
                ')' => depth = depth.saturating_sub(1),
                _ => return depth == 0 && !character.is_whitespace(),
            }
            false
        })
        .count()
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;
    use crate::subtitles::localization::parse_sections;
    use crate::subtitles::map::MapRecord;
    use crate::subtitles::tests::{test_bank, test_maps, test_records};

    fn test_call(sound_event: &str, key: u32, cue: Option<&str>) -> BridgeCall {
        BridgeCall {
            source_object: "State'EchelonPattern.P_Talk.Pattern'".to_owned(),
            sound_event: sound_event.to_owned(),
            sound_map_key: Some(key),
            section: cue.map(|_| "P_Talk".to_owned()),
            cue: cue.map(str::to_owned),
            function: Some(
                if cue.is_some() {
                    "__NFUN_1407__"
                } else {
                    "__NFUN_1408__"
                }
                .to_owned(),
            ),
        }
    }

    #[test]
    fn skips_the_unspoken_line_whose_removal_evens_out_the_speaking_rate() {
        // The scene speaks the last three lines; "What's that?" is never played.
        let clips = [(1, 0, 2.21), (2, 100, 2.72), (3, 200, 1.54)];
        let mut banks = test_bank(&[0, 100, 200]);
        let mut records = HashMap::new();
        for (index, &(key, offset, seconds)) in clips.iter().enumerate() {
            let sample_count = (seconds * 36000.0) as u32;
            banks[0].segments[index].sample_count = sample_count;
            records.insert(
                key,
                HashSet::from([MapRecord {
                    resource_key: key,
                    offset,
                    compressed_bytes: 10,
                    sample_count,
                    sample_rate: 36000,
                    bank: "1_1_2.LS0".to_owned(),
                }]),
            );
        }
        let pattern = BridgePattern {
            pattern: "P_Talk".to_owned(),
            source_package: "EchelonPattern.u".to_owned(),
            calls: vec![
                test_call("S1_1_2Voice.Play_A", 1, None),
                test_call("S1_1_2Voice.Play_B", 2, None),
                test_call("S1_1_2Voice.Play_C", 3, None),
            ],
        };
        let mut sections = HashMap::new();
        parse_sections(
            "[P_Talk]\nSpeech_0005L=SOLDIER A -  Whats that?\nSpeech_0001L=SOLDIER A -  I thought this was what made the pools drain.\nSpeech_0002L=SOLDIER B -  It is, I think.  Maybe it's malfunctioning.\nSpeech_0003L=SOLDIER A -  It doesn't effect us anyway.\n",
            &mut sections,
        );
        let mut analysis = SubtitleAnalysis::default();

        match_unshown_subtitles(
            &mut analysis,
            &pattern,
            &sections,
            &sections,
            &test_maps(records, &banks),
        );

        assert_eq!(analysis.unshown_pairs, 3);
        assert_eq!(
            analysis.by_segment["ENGLISH/1_1_2.LS0:0"][0].cue,
            "Speech_0001L"
        );
        assert_eq!(
            analysis.by_segment["ENGLISH/1_1_2.LS0:200"][0].cue,
            "Speech_0003L"
        );
    }

    #[test]
    fn aligns_against_english_and_ignores_leftover_lines_in_other_languages() {
        let banks = test_bank(&[0, 100]);
        let records = test_records(&[(1, 0), (2, 100)]);
        let pattern = BridgePattern {
            pattern: "P_Talk".to_owned(),
            source_package: "EchelonPattern.u".to_owned(),
            calls: vec![
                test_call("S1_1_2Voice.Play_A", 1, None),
                test_call("S1_1_2Voice.Play_B", 2, None),
            ],
        };
        let mut english = HashMap::new();
        parse_sections(
            "[P_Talk]\nSpeech_0001L=Damn.\nSpeech_0002L=Stop him.\n",
            &mut english,
        );
        let mut german = HashMap::new();
        parse_sections(
            "[P_Talk]\nSpeech_0001L=Verdammt.\nSpeech_0002L=Haltet ihn auf.\nSpeech_0015L=Ein alter Entwurf.\n",
            &mut german,
        );
        let mut analysis = SubtitleAnalysis::default();

        match_unshown_subtitles(
            &mut analysis,
            &pattern,
            &german,
            &english,
            &test_maps(records, &banks),
        );

        assert_eq!(analysis.unshown_pairs, 2);
        assert_eq!(
            analysis.by_segment["ENGLISH/1_1_2.LS0:0"][0]
                .text
                .as_deref(),
            Some("Verdammt.")
        );
        assert_eq!(
            analysis.by_segment["ENGLISH/1_1_2.LS0:100"][0]
                .text
                .as_deref(),
            Some("Haltet ihn auf.")
        );
    }

    #[test]
    fn keeps_lines_without_text_matched_to_their_audio() {
        let banks = test_bank(&[0, 100]);
        let records = test_records(&[(1, 0), (2, 100)]);
        let pattern = BridgePattern {
            pattern: "P_Talk".to_owned(),
            source_package: "EchelonPattern.u".to_owned(),
            calls: vec![
                test_call("S1_1_2Voice.Play_A", 1, None),
                test_call("S1_1_2Voice.Play_B", 2, None),
            ],
        };
        let mut sections = HashMap::new();
        parse_sections(
            "[P_Talk]\nSpeech_0001L=(null)\nSpeech_0002L=Hello.\n",
            &mut sections,
        );
        let mut analysis = SubtitleAnalysis::default();

        match_unshown_subtitles(
            &mut analysis,
            &pattern,
            &sections,
            &sections,
            &test_maps(records, &banks),
        );

        let empty = &analysis.by_segment["ENGLISH/1_1_2.LS0:0"][0];
        assert_eq!(
            (empty.cue.as_str(), empty.text.as_deref()),
            ("Speech_0001L", None)
        );
        assert_eq!(analysis.segments_with_text(), 1);
        assert_eq!(analysis.segments_with_empty_lines(), 1);
    }

    #[test]
    fn counts_only_spoken_characters() {
        assert_eq!(spoken_length("GSOLD -  (interrupting) We're going."), 11);
        assert_eq!(spoken_length("Um...Okay."), 10);
        assert_eq!(spoken_length("CIA Agent - 28469"), 14);
    }

    #[test]
    fn lists_every_combination_up_to_a_limit() {
        assert_eq!(
            combinations(4, 2, 10).unwrap(),
            [[0, 1], [0, 2], [0, 3], [1, 2], [1, 3], [2, 3]]
        );
        assert_eq!(combinations(3, 0, 10).unwrap(), [Vec::<usize>::new()]);
        assert!(combinations(40, 4, 200).is_none());
        assert_eq!(combinations(10, 9, 200).unwrap().len(), 10);
    }

    #[test]
    fn skips_sound_effects_when_aligning_dialogue() {
        let banks = test_bank(&[0, 100]);
        let records = test_records(&[(1, 0), (2, 100)]);
        let effect = BridgeCall {
            function: Some("__NFUN_264__".to_owned()),
            ..test_call("DestroyableObjet.Play_ExplosionBridge", 9, None)
        };
        let pattern = BridgePattern {
            pattern: "P_Talk".to_owned(),
            source_package: "EchelonPattern.u".to_owned(),
            calls: vec![
                test_call("S1_1_2Voice.Play_A", 1, None),
                effect,
                test_call("S1_1_2Voice.Play_B", 2, None),
            ],
        };
        let mut sections = HashMap::new();
        parse_sections(
            "[P_Talk]\nSpeech_0001L=What was that?\nSpeech_0002L=We're being attacked.\n",
            &mut sections,
        );
        let mut analysis = SubtitleAnalysis::default();

        match_unshown_subtitles(
            &mut analysis,
            &pattern,
            &sections,
            &sections,
            &test_maps(records, &banks),
        );

        assert_eq!(analysis.unshown_pairs, 2);
        assert_eq!(
            analysis.by_segment["ENGLISH/1_1_2.LS0:100"][0]
                .text
                .as_deref(),
            Some("We're being attacked.")
        );
    }

    #[test]
    fn matches_unshown_sounds_to_section_lines_by_position() {
        let banks = test_bank(&[0, 100, 200]);
        let records = test_records(&[(1, 0), (2, 100), (3, 200)]);
        let pattern = BridgePattern {
            pattern: "P_Talk".to_owned(),
            source_package: "EchelonPattern.u".to_owned(),
            calls: vec![
                test_call("S1_1_2Voice.Play_A", 1, None),
                test_call("S1_1_2Voice.Play_B", 2, Some("Speech_0005L")),
                test_call("S1_1_2Voice.Play_C", 3, None),
            ],
        };
        let mut sections = HashMap::new();
        parse_sections(
            "[P_Talk]\nSpeech_0009L=Hello.\nSpeech_0005L=Shown.\nSpeech_0001L=Goodbye.\n",
            &mut sections,
        );
        let mut analysis = SubtitleAnalysis::default();

        match_unshown_subtitles(
            &mut analysis,
            &pattern,
            &sections,
            &sections,
            &test_maps(records, &banks),
        );

        assert_eq!(analysis.unshown_pairs, 2);
        let first = &analysis.by_segment["ENGLISH/1_1_2.LS0:0"][0];
        assert_eq!(
            (first.cue.as_str(), first.text.as_deref()),
            ("Speech_0009L", Some("Hello."))
        );
        assert_eq!(first.confidence, MappingConfidence::SectionOrder);
        assert_eq!(
            analysis.by_segment["ENGLISH/1_1_2.LS0:200"][0]
                .text
                .as_deref(),
            Some("Goodbye.")
        );
        // The sound passed with its cue is matched explicitly elsewhere.
        assert!(!analysis.by_segment.contains_key("ENGLISH/1_1_2.LS0:100"));
        let lines = analysis
            .unshown_lines
            .iter()
            .map(|line| {
                (
                    line.sound_event.as_str(),
                    line.cue.as_str(),
                    line.segments.as_slice(),
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(
            lines,
            [
                (
                    "S1_1_2Voice.Play_A",
                    "Speech_0009L",
                    &["ENGLISH/1_1_2.LS0:0".to_owned()][..]
                ),
                (
                    "S1_1_2Voice.Play_C",
                    "Speech_0001L",
                    &["ENGLISH/1_1_2.LS0:200".to_owned()][..]
                ),
            ]
        );
    }

    /// The cues of the unshown lines a scene is matched to.
    fn unshown_cues(pattern: &BridgePattern, section: &str) -> Vec<(String, String)> {
        let banks = test_bank(&[0, 100, 200, 300]);
        let records = test_records(&[(1, 0), (2, 100), (3, 200), (4, 300)]);
        let mut sections = HashMap::new();
        parse_sections(section, &mut sections);
        let mut analysis = SubtitleAnalysis::default();

        match_unshown_subtitles(
            &mut analysis,
            pattern,
            &sections,
            &sections,
            &test_maps(records, &banks),
        );

        assert!(analysis.warnings.is_empty(), "{:?}", analysis.warnings);
        analysis
            .unshown_lines
            .iter()
            .map(|line| (line.sound_event.clone(), line.cue.clone()))
            .collect()
    }

    #[test]
    fn a_repeated_radio_message_takes_one_line() {
        // EnhancedSC's Hidebody06 plays a warning from a second branch, and
        // its section has the warning once.
        let pattern = BridgePattern {
            pattern: "P_Talk".to_owned(),
            source_package: "EchelonPattern.u".to_owned(),
            calls: vec![
                test_call("S1_1_2Voice.Play_A", 1, None),
                test_call("S1_1_2Voice.Play_B", 2, Some("Speech_0002L")),
                test_call("S1_1_2Voice.Play_B", 2, Some("Speech_0002L")),
                test_call("S1_1_2Voice.Play_C", 3, None),
            ],
        };

        let cues = unshown_cues(
            &pattern,
            "[P_Talk]\nSpeech_0001L=Heard you.\nSpeech_0002L=Try again.\nSpeech_0003L=Saw you.\n",
        );

        assert_eq!(
            cues,
            [
                ("S1_1_2Voice.Play_A".to_owned(), "Speech_0001L".to_owned()),
                ("S1_1_2Voice.Play_C".to_owned(), "Speech_0003L".to_owned()),
            ]
        );
    }

    #[test]
    fn a_radio_message_from_another_scene_takes_no_line() {
        // EnhancedSC's NikoCrisCurtain plays another scene's game over warning.
        let mut borrowed = test_call("S5_1_Voice.Play_X", 4, Some("Speech_0014L"));
        borrowed.section = Some("P_Other".to_owned());
        let pattern = BridgePattern {
            pattern: "P_Talk".to_owned(),
            source_package: "EchelonPattern.u".to_owned(),
            calls: vec![
                test_call("S1_1_2Voice.Play_A", 1, None),
                borrowed,
                test_call("S1_1_2Voice.Play_B", 2, None),
            ],
        };

        let cues = unshown_cues(
            &pattern,
            "[P_Talk]\nSpeech_0001L=Hello.\nSpeech_0002L=Goodbye.\n",
        );

        assert_eq!(
            cues,
            [
                ("S1_1_2Voice.Play_A".to_owned(), "Speech_0001L".to_owned()),
                ("S1_1_2Voice.Play_B".to_owned(), "Speech_0002L".to_owned()),
            ]
        );
    }

    #[test]
    fn counts_repeated_sounds_in_a_source_object() {
        let banks = test_bank(&[0]);
        let records = test_records(&[(1, 0)]);
        let pattern = BridgePattern {
            pattern: "P_Talk".to_owned(),
            source_package: "EchelonPattern.u".to_owned(),
            calls: vec![
                test_call("S1_1_2Voice.Play_A", 1, None),
                test_call("S1_1_2Voice.Play_A", 1, None),
            ],
        };
        let mut sections = HashMap::new();
        parse_sections(
            "[P_Talk]\nSpeech_0001L=First.\nSpeech_0002L=Again.\n",
            &mut sections,
        );
        let mut analysis = SubtitleAnalysis::default();

        match_unshown_subtitles(
            &mut analysis,
            &pattern,
            &sections,
            &sections,
            &test_maps(records, &banks),
        );

        let occurrences = analysis
            .unshown_lines
            .iter()
            .map(|line| (line.cue.as_str(), line.occurrence))
            .collect::<Vec<_>>();
        assert_eq!(occurrences, [("Speech_0001L", 0), ("Speech_0002L", 1)]);
    }

    #[test]
    fn skips_section_lines_that_arent_spoken() {
        let banks = test_bank(&[0, 100]);
        let records = test_records(&[(1, 0), (2, 100)]);
        let pattern = BridgePattern {
            pattern: "P_Talk".to_owned(),
            source_package: "EchelonPattern.u".to_owned(),
            calls: vec![
                test_call("S1_1_2Voice.Play_A", 1, None),
                test_call("S1_1_2Voice.Play_B", 2, None),
            ],
        };
        let mut sections = HashMap::new();
        parse_sections(
            "[P_Talk]\nSpeech_0001L=What's the code?\nNote_0002L=The code is 28469.\nSpeech_0003L=28469.\n",
            &mut sections,
        );
        let mut analysis = SubtitleAnalysis::default();

        match_unshown_subtitles(
            &mut analysis,
            &pattern,
            &sections,
            &sections,
            &test_maps(records, &banks),
        );

        assert_eq!(analysis.unshown_pairs, 2);
        assert_eq!(
            analysis.by_segment["ENGLISH/1_1_2.LS0:100"][0]
                .text
                .as_deref(),
            Some("28469.")
        );
    }

    #[test]
    fn skips_patterns_whose_lines_dont_line_up() {
        let banks = test_bank(&[0, 100]);
        let maps = test_maps(test_records(&[(1, 0), (2, 100)]), &banks);
        let mut sections = HashMap::new();
        parse_sections(
            "[P_Talk]\nSpeech_0001L=One.\nSpeech_0002L=Two.\nSpeech_0003L=Six.\n",
            &mut sections,
        );
        let fewer_sounds = BridgePattern {
            pattern: "P_Talk".to_owned(),
            source_package: "EchelonPattern.u".to_owned(),
            calls: vec![
                test_call("S1_1_2Voice.Play_A", 1, None),
                test_call("S1_1_2Voice.Play_B", 2, None),
            ],
        };
        let misplaced_cue = BridgePattern {
            pattern: "P_Talk".to_owned(),
            source_package: "EchelonPattern.u".to_owned(),
            calls: vec![
                test_call("S1_1_2Voice.Play_A", 1, Some("Speech_0002L")),
                test_call("S1_1_2Voice.Play_B", 2, None),
                test_call("S1_1_2Voice.Play_C", 2, None),
            ],
        };
        let mut analysis = SubtitleAnalysis::default();

        for pattern in [&fewer_sounds, &misplaced_cue] {
            match_unshown_subtitles(&mut analysis, pattern, &sections, &sections, &maps);
        }

        assert_eq!(analysis.unshown_pairs, 0);
        assert!(analysis.by_segment.is_empty());
        assert_eq!(analysis.warnings.len(), 2);
    }
}
