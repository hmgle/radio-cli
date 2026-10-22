//! Display-only formatting shared by lists, details and the player header.

/// `MM:SS`, or `H:MM:SS` from one hour up.
pub(super) fn duration(ms: u64) -> String {
    let seconds = ms / 1000;
    let (hours, minutes, seconds) = (seconds / 3600, seconds / 60 % 60, seconds % 60);
    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes:02}:{seconds:02}")
    }
}

/// A readable size that keeps the exact byte count.
pub(super) fn size(bytes: u64) -> String {
    let exact = crate::t!("size_bytes", "count" => bytes);
    if bytes < 1024 {
        return exact;
    }
    let mut value = bytes as f64;
    let mut unit = "";
    for next in ["KiB", "MiB", "GiB", "TiB"] {
        value /= 1024.0;
        unit = next;
        if value < 1024.0 {
            break;
        }
    }
    format!("{value:.1} {unit} ({exact})")
}

/// Persisted recording states keep their stored text; unknown values show as stored.
pub(super) fn recording_status(status: &str) -> String {
    match status {
        "recording" => crate::t!("recording_status_recording").into(),
        "finalizing" => crate::t!("recording_status_finalizing").into(),
        "complete" => crate::t!("recording_status_complete").into(),
        "interrupted" => crate::t!("recording_status_interrupted").into(),
        "failed" => crate::t!("recording_status_failed").into(),
        other => crate::clean_text(other),
    }
}

/// Only states that are still running or being finalized have a pending end time.
pub(super) fn recording_stop_time(status: &str, stopped_at: Option<&str>) -> String {
    match (stopped_at, status) {
        (Some(time), _) => crate::clean_text(time),
        (None, "recording") => crate::t!("recording_status_recording").into(),
        (None, "finalizing") => crate::t!("recording_status_finalizing").into(),
        (None, _) => crate::t!("value_unknown").into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_use_minutes_and_hours() {
        for (ms, expected) in [
            (0, "00:00"),
            (999, "00:00"),
            (1_000, "00:01"),
            (65_400, "01:05"),
            (3_599_999, "59:59"),
            (3_600_000, "1:00:00"),
            (36_061_000, "10:01:01"),
        ] {
            assert_eq!(duration(ms), expected, "{ms}");
        }
    }

    #[test]
    fn sizes_are_readable_and_keep_the_exact_count() {
        assert_eq!(size(0), crate::t!("size_bytes", "count" => 0));
        assert_eq!(size(1023), crate::t!("size_bytes", "count" => 1023));
        for (bytes, human) in [
            (1024, "1.0 KiB ("),
            (1536, "1.5 KiB ("),
            (1_048_576, "1.0 MiB ("),
            (5 * 1024 * 1024 * 1024, "5.0 GiB ("),
            (3 * 1024 * 1024 * 1024 * 1024, "3.0 TiB ("),
            (u64::MAX, "16777216.0 TiB ("),
        ] {
            let text = size(bytes);
            assert!(text.starts_with(human), "{text}");
            assert!(text.contains(&bytes.to_string()), "{text}");
        }
    }

    #[test]
    fn known_recording_states_are_localized_and_unknown_ones_are_sanitized() {
        for (stored, label) in [
            ("recording", crate::t!("recording_status_recording")),
            ("finalizing", crate::t!("recording_status_finalizing")),
            ("complete", crate::t!("recording_status_complete")),
            ("interrupted", crate::t!("recording_status_interrupted")),
            ("failed", crate::t!("recording_status_failed")),
        ] {
            assert_eq!(recording_status(stored), label);
        }
        assert_eq!(recording_status("archived\x1b[2J"), "archived[2J");
    }

    #[test]
    fn only_running_states_claim_a_pending_stop_time() {
        let unknown = crate::t!("value_unknown");
        assert_eq!(
            recording_stop_time("complete", Some("2026-01-01")),
            "2026-01-01"
        );
        assert_eq!(
            recording_stop_time("recording", None),
            crate::t!("recording_status_recording")
        );
        assert_eq!(
            recording_stop_time("finalizing", None),
            crate::t!("recording_status_finalizing")
        );
        for status in ["interrupted", "failed", "complete", "other"] {
            assert_eq!(recording_stop_time(status, None), unknown, "{status}");
        }
    }
}
