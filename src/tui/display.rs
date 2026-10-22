//! Display-only formatting shared by lists, details and the player header.

use crate::engine::PlaybackState;
use ratatui::style::{Color, Style};

/// Semantic class of a media state. Colour only reinforces the glyph and the
/// text label, which remain readable in monochrome output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Tone {
    Idle,
    Active,
    Pending,
    Failed,
}

impl Tone {
    pub(super) fn style(self) -> Style {
        match self {
            Self::Idle => Style::default(),
            Self::Active => Style::default().fg(Color::Green),
            Self::Pending => Style::default().fg(Color::Yellow),
            Self::Failed => Style::default().fg(Color::Red),
        }
    }
}

pub(super) fn playback_tone(state: &PlaybackState) -> Tone {
    match state {
        PlaybackState::Idle => Tone::Idle,
        PlaybackState::Playing => Tone::Active,
        PlaybackState::Paused | PlaybackState::Connecting | PlaybackState::Reconnecting { .. } => {
            Tone::Pending
        }
        PlaybackState::Failed { .. } => Tone::Failed,
    }
}

/// One-cell marker shared by the player header, the list rows and the details
/// header. Idle has none.
pub(super) fn playback_glyph(state: &PlaybackState) -> Option<&'static str> {
    match state {
        PlaybackState::Idle => None,
        PlaybackState::Playing => Some("▶"),
        PlaybackState::Paused => Some("‖"),
        PlaybackState::Connecting => Some("…"),
        PlaybackState::Reconnecting { .. } => Some("↻"),
        PlaybackState::Failed { .. } => Some("!"),
    }
}

/// The state's text label. Reconnecting names the attempt; a failure carries
/// its reason cut to `reason_width` cells (0 omits the reason).
pub(super) fn playback_label(state: &PlaybackState, reason_width: usize) -> String {
    match state {
        PlaybackState::Reconnecting { attempt } => {
            crate::t!("state_playback_reconnecting_attempt", "attempt" => attempt)
        }
        PlaybackState::Failed { reason } if reason_width > 0 => {
            crate::t!("playback_failed", "reason" => super::render::fit(reason, reason_width).trim_end())
        }
        state => state.label().into(),
    }
}

/// Glyph and label together, as shown in the player header and details.
pub(super) fn playback_status(state: &PlaybackState, reason_width: usize) -> String {
    let label = playback_label(state, reason_width);
    match playback_glyph(state) {
        Some(glyph) => format!("{glyph} {label}"),
        None => label,
    }
}

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

    fn states() -> [PlaybackState; 6] {
        [
            PlaybackState::Idle,
            PlaybackState::Connecting,
            PlaybackState::Reconnecting { attempt: 2 },
            PlaybackState::Playing,
            PlaybackState::Paused,
            PlaybackState::Failed {
                reason: "HTTP 503 from the upstream station".into(),
            },
        ]
    }

    #[test]
    fn every_active_state_has_its_own_one_cell_glyph() {
        let glyphs: Vec<_> = states().iter().map(playback_glyph).collect();
        assert_eq!(glyphs[0], None);
        let mut seen = std::collections::HashSet::new();
        for glyph in glyphs.into_iter().flatten() {
            assert_eq!(super::super::text::width(glyph), 1, "{glyph}");
            assert!(seen.insert(glyph), "{glyph} is shared by two states");
        }
        assert_eq!(seen.len(), 5);
    }

    #[test]
    fn tones_group_states_by_how_much_attention_they_need() {
        let tones: Vec<_> = states().iter().map(playback_tone).collect();
        assert_eq!(
            tones,
            [
                Tone::Idle,
                Tone::Pending,
                Tone::Pending,
                Tone::Active,
                Tone::Pending,
                Tone::Failed
            ]
        );
        let styles: std::collections::HashSet<_> = [Tone::Active, Tone::Pending, Tone::Failed]
            .into_iter()
            .map(|tone| format!("{:?}", tone.style()))
            .collect();
        assert_eq!(styles.len(), 3);
    }

    #[test]
    fn labels_name_the_attempt_and_cut_failures_to_the_budget() {
        let [idle, _, reconnecting, playing, _, failed] = states();
        assert_eq!(playback_status(&idle, 12), idle.label());
        assert_eq!(playback_label(&playing, 12), playing.label());
        let attempt = playback_label(&reconnecting, 12);
        assert!(attempt.starts_with(reconnecting.label()), "{attempt}");
        assert!(attempt.contains('2'), "{attempt}");
        assert!(playback_status(&playing, 12).starts_with("▶ "));

        let prefix = super::super::text::width(&playback_label(
            &PlaybackState::Failed { reason: "".into() },
            1,
        ));
        for budget in [1, 12, 20] {
            let label = playback_label(&failed, budget);
            assert!(
                super::super::text::width(&label) <= prefix + budget,
                "{budget}: {label}"
            );
        }
        assert_eq!(playback_label(&failed, 0), failed.label());
        assert!(playback_label(&failed, 80).contains("upstream station"));
    }

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
