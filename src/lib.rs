mod artwork;
pub mod cli;
pub mod config;
pub mod directory;
pub mod engine;
pub mod error;
pub mod messages;
pub mod model;
pub mod player;
pub mod process;
pub mod recorder;
pub mod service;
pub mod store;
mod territories;
pub mod tui;

use unicode_segmentation::UnicodeSegmentation;
pub const USER_AGENT: &str = concat!("radio-cli/", env!("CARGO_PKG_VERSION"));

const CLEAN_TEXT_LIMIT: usize = 1024;

/// External text must never introduce terminal escape sequences or new lines.
///
/// The length limit counts scalar values but never cuts through a grapheme
/// cluster, so a joiner or combining mark is not left dangling at the end.
pub fn clean_text(text: &str) -> String {
    let mut cleaned = String::new();
    let mut count = 0;
    for grapheme in text.graphemes(true) {
        // Control characters always form their own cluster, including CR LF.
        if grapheme.chars().next().is_none_or(char::is_control) {
            continue;
        }
        count += grapheme.chars().count();
        if count > CLEAN_TEXT_LIMIT {
            break;
        }
        cleaned.push_str(grapheme);
    }
    cleaned
}

#[cfg(test)]
mod clean_text_tests {
    use super::*;

    #[test]
    fn clean_text_strips_controls_and_caps_on_a_grapheme_boundary() {
        assert_eq!(clean_text("a\u{1b}[31mb\r\nc\u{7f}"), "a[31mbc");
        assert_eq!(clean_text("你好 ✓"), "你好 ✓");
        let long = "x".repeat(CLEAN_TEXT_LIMIT + 10);
        assert_eq!(clean_text(&long).chars().count(), CLEAN_TEXT_LIMIT);

        // The cluster is five scalar values and would cross the limit: drop it whole.
        let family = "👨\u{200d}👩\u{200d}👧";
        let text = format!("{}{family}tail", "x".repeat(CLEAN_TEXT_LIMIT - 2));
        assert_eq!(clean_text(&text), "x".repeat(CLEAN_TEXT_LIMIT - 2));

        // Exactly at the limit the cluster is kept intact.
        let text = format!("{}{family}tail", "x".repeat(CLEAN_TEXT_LIMIT - 5));
        assert_eq!(
            clean_text(&text),
            format!("{}{family}", "x".repeat(CLEAN_TEXT_LIMIT - 5))
        );

        let accents = format!("e{}", "\u{301}".repeat(CLEAN_TEXT_LIMIT));
        assert_eq!(clean_text(&accents), "");
    }
}

#[cfg(test)]
extern crate self as radio_cli;
#[cfg(test)]
#[path = "../tests/support/mod.rs"]
mod test_support;
