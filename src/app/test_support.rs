//! Banks and segments for tests.

use std::path::PathBuf;
use std::sync::Arc;

use sc_mod_tool_core::convert::FitOptions;
use sc_mod_tool_core::decoder::DecodedAudio;
use sc_mod_tool_core::{Bank, Segment, SegmentSource, SoundKind};

use super::edits::Replacement;
use crate::audio::PlayableAudio;

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

/// A segment of `sample_count` samples at 36 kHz in the `.LS0` bank at
/// `bank_path`, at `offset`.
pub(super) fn stream_segment(bank_path: &str, offset: u64, sample_count: u32) -> Segment {
    Segment {
        id: format!("{bank_path}:{offset}"),
        source: SegmentSource::Stream {
            offset,
            byte_length: 100,
        },
        ..segment(sample_count, 36000)
    }
}

/// A replacement of `sample_count` samples of silence for the stream at
/// `offset`.
pub(super) fn replacement(offset: u64, sample_count: u32) -> Replacement {
    Replacement {
        source: PathBuf::from("line.wav"),
        fit: FitOptions::default(),
        offset,
        stream: Arc::from(vec![offset as u8; 4]),
        sample_count,
        changes_length: false,
        preview: PlayableAudio::new(DecodedAudio {
            samples: vec![0; sample_count as usize],
            sample_rate: 36000,
            channels: 1,
        })
        .unwrap(),
    }
}
