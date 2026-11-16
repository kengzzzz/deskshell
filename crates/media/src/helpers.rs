//! Display helpers for the volume, mic and time labels.

use std::time::Duration;

pub fn volume_icon(volume: f32, muted: bool) -> &'static str {
    if muted || volume <= 0.0 {
        "volume_off"
    } else if volume < 0.5 {
        "volume_down"
    } else {
        "volume_up"
    }
}

pub fn mic_icon(muted: bool) -> &'static str {
    if muted { "mic_off" } else { "mic" }
}

pub fn percent_label(volume: f32, muted: bool) -> String {
    if muted {
        return "Muted".to_owned();
    }
    let percent = (f64::from(volume.clamp(0.0, 1.0)) * 100.0 + 0.5).floor();
    format!("{percent}%")
}

pub fn format_time(time: Duration) -> String {
    let total = time.as_secs();
    let (hours, minutes, seconds) = (total / 3600, total / 60 % 60, total % 60);
    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes}:{seconds:02}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn volume_icons() {
        assert_eq!(volume_icon(0.0, false), "volume_off");
        assert_eq!(volume_icon(0.8, true), "volume_off");
        assert_eq!(volume_icon(0.49, false), "volume_down");
        assert_eq!(volume_icon(0.5, false), "volume_up");
        assert_eq!(volume_icon(1.5, false), "volume_up");
        assert_eq!(mic_icon(true), "mic_off");
        assert_eq!(mic_icon(false), "mic");
    }

    #[test]
    fn percent_labels() {
        assert_eq!(percent_label(0.5, true), "Muted");
        assert_eq!(percent_label(0.0, false), "0%");
        assert_eq!(percent_label(0.375, false), "38%");
        assert_eq!(percent_label(0.374, false), "37%");
        assert_eq!(percent_label(0.995, false), "100%");
        assert_eq!(percent_label(1.5, false), "100%");
    }

    #[test]
    fn times() {
        assert_eq!(format_time(Duration::ZERO), "0:00");
        assert_eq!(format_time(Duration::from_millis(59_999)), "0:59");
        assert_eq!(format_time(Duration::from_secs(61)), "1:01");
        assert_eq!(format_time(Duration::from_secs(3599)), "59:59");
        assert_eq!(format_time(Duration::from_secs(3600)), "1:00:00");
        assert_eq!(
            format_time(Duration::from_secs(36_005)),
            "10:00:05"
        );
    }
}
