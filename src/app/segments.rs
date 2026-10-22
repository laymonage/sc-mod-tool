//! The segment table's rows: which of the selected bank's segments it shows,
//! and in what order. Playback follows the same order.

use sc_mod_tool_core::{Bank, Segment, SegmentSource, subtitles::Subtitle};

use super::format::format_key;
use super::{SCModToolApp, SortColumn, TextFilter};

impl SCModToolApp {
    /// The segment `offset` places from `segment_id` in the segment table, as
    /// it's filtered and sorted. `None` past either end, or when the segment
    /// isn't in the selected bank, such as after choosing another bank.
    pub(super) fn adjacent_segment(&self, segment_id: &str, offset: isize) -> Option<usize> {
        let bank = self.selected_bank()?;
        let visible = self.visible_segments();
        let position = visible
            .iter()
            .position(|&index| bank.segments[index].id == segment_id)?;
        visible.get(position.checked_add_signed(offset)?).copied()
    }

    /// The selected bank's segments that match the search and filters.
    pub(super) fn visible_segments(&self) -> Vec<usize> {
        let Some(bank) = self.selected_bank() else {
            return Vec::new();
        };
        let query = self.segment_filter.trim().to_lowercase();
        // The text filter is only offered for banks with subtitles.
        let filters_text = self.has_subtitles(bank);
        let visible = bank
            .segments
            .iter()
            .enumerate()
            .filter(|(_, segment)| {
                let subtitles = self.subtitles.get(&segment.id);
                let has_text =
                    subtitles.is_some_and(|subtitles| subtitles.iter().any(Subtitle::has_text));
                let shown = !filters_text
                    || match self.text_filter {
                        TextFilter::All => true,
                        TextFilter::WithText => has_text,
                        TextFilter::WithoutText => !has_text,
                    };
                let subtitle_matches = subtitles.is_some_and(|subtitles| {
                    subtitles.iter().any(|subtitle| {
                        subtitle
                            .text
                            .as_deref()
                            .is_some_and(|text| text.to_lowercase().contains(&query))
                            || subtitle.cue.to_lowercase().contains(&query)
                    })
                });
                // Map sounds can also be found by name, key and level.
                let sound_matches = match &segment.source {
                    SegmentSource::MapSound { key, areas, .. } => {
                        segment
                            .name
                            .as_deref()
                            .is_some_and(|name| name.to_lowercase().contains(&query))
                            || format_key(*key).to_lowercase().contains(&query)
                            || areas.iter().any(|area| {
                                [Some(&area.name), area.mission_name.as_ref()]
                                    .into_iter()
                                    .flatten()
                                    .any(|text| text.to_lowercase().contains(&query))
                            })
                    }
                    SegmentSource::Stream { .. } => false,
                };
                shown && (query.is_empty() || subtitle_matches || sound_matches)
            })
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        // Banks without the sorted column keep their own order.
        let sort = self.segment_sort;
        if !table_columns(bank.sound_kind.is_some(), filters_text).contains(&sort.column) {
            return visible;
        }
        let mut keyed = visible
            .into_iter()
            .map(|index| {
                let segment = &bank.segments[index];
                (self.sort_key(&bank.path, segment, sort.column), index)
            })
            .collect::<Vec<_>>();
        // Missing values go last either way, and segments with equal values
        // stay in the bank's order.
        keyed.sort_by(
            |((left_missing, left), left_index), ((right_missing, right), right_index)| {
                let values = if sort.descending {
                    right.cmp(left)
                } else {
                    left.cmp(right)
                };
                left_missing
                    .cmp(right_missing)
                    .then(values)
                    .then(left_index.cmp(right_index))
            },
        );
        keyed.into_iter().map(|(_, index)| index).collect()
    }

    /// A segment of the bank at `bank_path`'s value in `column`, after a flag
    /// that's set when it has none, such as a stream's key.
    fn sort_key(
        &self,
        bank_path: &str,
        segment: &Segment,
        column: SortColumn,
    ) -> (bool, SortValue) {
        let number = |value: Option<u64>| (value.is_none(), SortValue::Number(value.unwrap_or(0)));
        let text = |value: Option<String>| {
            (
                value.is_none(),
                SortValue::Text(value.unwrap_or_default().to_lowercase()),
            )
        };
        let (subsong, key, areas) = match &segment.source {
            SegmentSource::MapSound {
                subsong,
                key,
                areas,
                ..
            } => (Some(*subsong), *key, Some(areas.len())),
            SegmentSource::Stream { .. } => (None, None, None),
        };
        match column {
            SortColumn::Number => number(Some(segment.segment as u64)),
            // As shown: a replacement's length, if it has one.
            SortColumn::Duration => {
                let sample_count = self
                    .replacement(bank_path, &segment.id)
                    .map_or(segment.sample_count, |replacement| replacement.sample_count);
                number(Some(
                    u64::from(sample_count) * 1_000_000 / u64::from(segment.sample_rate.max(1)),
                ))
            }
            SortColumn::Size => number(segment.stream_range().map(|(_, length)| length)),
            SortColumn::Offset => number(segment.stream_range().map(|(offset, _)| offset)),
            SortColumn::UsedIn => number(areas.map(|count| count as u64)),
            SortColumn::Entry => number(subsong.map(u64::from)),
            SortColumn::Key => number(key.map(u64::from)),
            SortColumn::Name => text(segment.name.clone()),
            // As shown: the first subtitle's text, or its cue when it has none.
            SortColumn::Subtitle => text(self.subtitles.get(&segment.id).map(|subtitles| {
                subtitles[0]
                    .text
                    .clone()
                    .unwrap_or_else(|| subtitles[0].cue.clone())
            })),
        }
    }

    /// Whether any of the bank's segments has a subtitle. `.LS0` banks are
    /// treated as having them, since that's what they're for.
    pub(super) fn has_subtitles(&self, bank: &Bank) -> bool {
        bank.sound_kind.is_none()
            || bank
                .segments
                .iter()
                .any(|segment| self.subtitles.contains_key(&segment.id))
    }

    pub(super) fn find_segment(&self, id: &str) -> Option<(&Bank, &Segment)> {
        self.banks.iter().find_map(|bank| {
            bank.segments
                .iter()
                .find(|segment| segment.id == id)
                .map(|segment| (bank, segment))
        })
    }
}

/// The segment table's columns for an `.LS0` bank or a sound map's bank, after
/// its play button. Every column can be sorted by. Map sounds' names come
/// before their subtitles, when they have any.
pub(super) fn table_columns(is_map_bank: bool, has_subtitles: bool) -> Vec<SortColumn> {
    let mut columns = vec![SortColumn::Number, SortColumn::Duration];
    if is_map_bank {
        columns.extend([
            SortColumn::UsedIn,
            SortColumn::Entry,
            SortColumn::Key,
            SortColumn::Name,
        ]);
        if has_subtitles {
            columns.push(SortColumn::Subtitle);
        }
    } else {
        columns.extend([SortColumn::Size, SortColumn::Offset, SortColumn::Subtitle]);
    }
    columns
}

/// A value segments are sorted by.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
enum SortValue {
    Number(u64),
    Text(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::test_support::{bank, segment};

    #[test]
    fn keeps_a_banks_order_when_it_lacks_the_sorted_column() {
        let mut app = SCModToolApp::new(None);
        let mut bank = bank("ENGLISH/1_1_0.LS0", None, None);
        bank.segments = [3000, 1000, 2000]
            .into_iter()
            .enumerate()
            .map(|(number, sample_count)| Segment {
                id: format!("ENGLISH/1_1_0.LS0:{number}"),
                segment: number,
                ..segment(sample_count, 1000)
            })
            .collect();
        app.banks = vec![bank];
        app.selected_bank = Some(0);

        app.segment_sort = app.segment_sort.by(SortColumn::Duration);
        assert_eq!(app.visible_segments(), [1, 2, 0]);
        // A replacement sorts by its own length.
        app.edits.insert(
            "ENGLISH/1_1_0.LS0",
            "ENGLISH/1_1_0.LS0:1",
            crate::app::test_support::replacement(0, 4000),
        );
        assert_eq!(app.visible_segments(), [2, 0, 1]);

        // `.LS0` banks have no Key column, such as after sorting a map's bank
        // by it and choosing another bank.
        app.segment_sort = app.segment_sort.by(SortColumn::Key);
        assert_eq!(app.visible_segments(), [0, 1, 2]);
    }

    #[test]
    fn skips_through_segments_in_the_tables_order() {
        let mut app = SCModToolApp::new(None);
        let mut bank = bank("ENGLISH/1_1_0.LS0", None, None);
        bank.segments = (0..3)
            .map(|number| Segment {
                id: format!("ENGLISH/1_1_0.LS0:{number}"),
                segment: number,
                ..segment(1000, 1000)
            })
            .collect();
        app.banks = vec![bank];
        app.selected_bank = Some(0);
        let id = |number| format!("ENGLISH/1_1_0.LS0:{number}");

        assert_eq!(app.adjacent_segment(&id(1), 1), Some(2));
        assert_eq!(app.adjacent_segment(&id(1), -1), Some(0));
        assert_eq!(app.adjacent_segment(&id(2), 1), None);
        assert_eq!(app.adjacent_segment(&id(0), -1), None);

        // Sorted the other way, next goes up the bank.
        app.segment_sort = app.segment_sort.by(SortColumn::Number);
        assert_eq!(app.adjacent_segment(&id(1), 1), Some(0));

        // A segment from another bank, such as after choosing a different one
        // while it plays.
        assert_eq!(app.adjacent_segment("ENGLISH/1_1_1.LS0:0", 1), None);
    }
}
