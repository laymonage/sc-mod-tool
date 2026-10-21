//! How values are written in the UI.

use sc_mod_tool_core::Segment;

/// Formats seconds as `m:ss`.
pub(super) fn format_time(seconds: f32) -> String {
    let seconds = seconds.max(0.0) as u32;
    format!("{}:{:02}", seconds / 60, seconds % 60)
}

pub(super) fn format_duration(segment: &Segment) -> String {
    let seconds = f64::from(segment.sample_count) / f64::from(segment.sample_rate.max(1));
    // Rounded to tenths before the minutes are split off, so 119.96 s is
    // 2:00.0, not 1:60.0.
    let tenths = (seconds * 10.0).round() as u64;
    if tenths < 600 {
        format!("{}.{} s", tenths / 10, tenths % 10)
    } else {
        format!("{}:{:02}.{}", tenths / 600, tenths / 10 % 60, tenths % 10)
    }
}

pub(super) fn format_channels(channels: u32) -> String {
    match channels {
        1 => "Mono".to_owned(),
        2 => "Stereo".to_owned(),
        _ => channels.to_string(),
    }
}

pub(super) fn format_key(key: Option<u32>) -> String {
    key.map_or_else(|| "—".to_owned(), |key| format!("0x{key:08X}"))
}

pub(super) fn format_bytes(bytes: u64) -> String {
    const KIB: f64 = 1024.0;
    let bytes = bytes as f64;
    if bytes < KIB {
        format!("{bytes} B")
    } else if bytes < KIB * KIB {
        format!("{:.1} KB", bytes / KIB)
    } else {
        format!("{:.1} MB", bytes / (KIB * KIB))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::test_support::segment;

    #[test]
    fn rounds_durations_before_splitting_off_minutes() {
        let duration = |milliseconds| format_duration(&segment(milliseconds, 1000));
        assert_eq!(duration(5_830), "5.8 s");
        assert_eq!(duration(59_940), "59.9 s");
        assert_eq!(duration(59_960), "1:00.0");
        assert_eq!(duration(119_960), "2:00.0");
        assert_eq!(duration(125_040), "2:05.0");
        assert_eq!(duration(3_599_960), "60:00.0");
    }
}
