//! Banks and segments for tests.

use sc_mod_tool_core::{Bank, Segment, SegmentSource, SoundKind};

pub(super) fn segment(sample_count: u32, sample_rate: u32) -> Segment {
    Segment {
        id: String::new(),
        source: SegmentSource::Stream {
            offset: 0,
            byte_length: 0,
        },
        name: None,
        sample_count,
        sample_rate,
        channels: 1,
        codec: String::new(),
        segment: 0,
    }
}

pub(super) fn bank(path: &str, map: Option<&str>, sound_kind: Option<SoundKind>) -> Bank {
    Bank {
        name: sound_kind.map_or_else(
            || path.rsplit('/').next().unwrap().to_owned(),
            |_| "Music".to_owned(),
        ),
        map: map.map(str::to_owned),
        sound_kind,
        path: path.to_owned(),
        code: String::new(),
        mission_name: None,
        size: 0,
        issue: None,
        segments: Vec::new(),
    }
}
