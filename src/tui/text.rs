//! Display-cell text helpers that never split a grapheme cluster.
//!
//! Widths follow `ratatui::buffer::CellWidth`, the measure used when the
//! buffer is painted, so what is computed here is what the terminal receives.

use ratatui::buffer::CellWidth;
use unicode_segmentation::UnicodeSegmentation;

/// Cells occupied by one grapheme cluster.
fn cells(grapheme: &str) -> usize {
    if grapheme.chars().next().is_none_or(char::is_control) {
        0
    } else {
        usize::from(grapheme.cell_width())
    }
}

pub(super) fn width(text: &str) -> usize {
    text.graphemes(true).map(cells).sum()
}

/// Longest prefix made of whole graphemes that fits in `budget` cells.
pub(super) fn prefix(text: &str, budget: usize) -> &str {
    let mut used = 0;
    for (index, grapheme) in text.grapheme_indices(true) {
        let size = cells(grapheme);
        if used + size > budget {
            return &text[..index];
        }
        used += size;
    }
    text
}

/// Exactly `width` cells: padded when short, ellipsized when long.
pub(super) fn fit(text: &str, width: usize) -> String {
    let text_width = self::width(text);
    if text_width <= width {
        return format!("{text}{}", " ".repeat(width - text_width));
    }
    if width == 0 {
        return String::new();
    }
    let head = prefix(text, width - 1);
    format!("{head}…{}", " ".repeat(width - self::width(head) - 1))
}

/// Hard-wrap by cells. Every line holds at least one grapheme, so a cluster
/// wider than `width` still gets a line of its own instead of being dropped.
pub(super) fn wrap(text: &str, width: usize) -> Vec<&str> {
    let width = width.max(1);
    let mut lines = Vec::new();
    let mut start = 0;
    let mut used = 0;
    for (index, grapheme) in text.grapheme_indices(true) {
        let size = cells(grapheme);
        if used + size > width && index > start {
            lines.push(&text[start..index]);
            start = index;
            used = 0;
        }
        used += size;
    }
    lines.push(&text[start..]);
    lines
}

/// Wrap at spaces, falling back to a hard break inside a word wider than the
/// line. Spaces at a break and at the start of the text are dropped.
pub(super) fn wrap_words(text: &str, width: usize) -> Vec<&str> {
    let width = width.max(1);
    let mut lines = Vec::new();
    let mut start = 0;
    let mut used = 0;
    // Where the latest run of spaces in the current line ends its text and where the next word resumes.
    let mut gap: Option<(usize, usize)> = None;
    let mut after_space = false;
    for (index, grapheme) in text.grapheme_indices(true) {
        if grapheme == " " {
            if index == start {
                start += 1;
                continue;
            }
            gap = match gap {
                Some((end, _)) if after_space => Some((end, index + 1)),
                _ => Some((index, index + 1)),
            };
            used += 1;
            after_space = true;
            continue;
        }
        after_space = false;
        let size = cells(grapheme);
        if used + size > width && index > start {
            match gap.take() {
                Some((end, resume)) => {
                    lines.push(&text[start..end]);
                    start = resume;
                    used = self::width(&text[start..index]);
                }
                None => {
                    lines.push(&text[start..index]);
                    start = index;
                    used = 0;
                }
            }
        }
        used += size;
    }
    lines.push(text[start..].trim_end_matches(' '));
    lines
}

/// Longest prefix of whole graphemes that fits in `limit` bytes.
pub(super) fn prefix_bytes(text: &str, limit: usize) -> &str {
    let mut end = 0;
    for (index, grapheme) in text.grapheme_indices(true) {
        if index + grapheme.len() > limit {
            break;
        }
        end = index + grapheme.len();
    }
    &text[..end]
}

/// Byte offset of the grapheme boundary before `index`, or 0.
pub(super) fn prev_boundary(text: &str, index: usize) -> usize {
    text.grapheme_indices(true)
        .map(|(start, _)| start)
        .take_while(|&start| start < index)
        .last()
        .unwrap_or(0)
}

/// Byte offset of the grapheme boundary after `index`, or the text length.
pub(super) fn next_boundary(text: &str, index: usize) -> usize {
    text.grapheme_indices(true)
        .map(|(start, grapheme)| start + grapheme.len())
        .find(|&end| end > index)
        .unwrap_or(text.len())
}

/// Horizontally scroll `text` so the cursor (a byte index on a grapheme
/// boundary) stays visible. Returns the visible slice and the cursor column.
pub(super) fn window(text: &str, cursor: usize, width: usize) -> (&str, usize) {
    if width == 0 {
        return ("", 0);
    }
    let mut start = 0;
    let mut column = self::width(&text[..cursor]);
    for grapheme in text[..cursor].graphemes(true) {
        if column < width {
            break;
        }
        start += grapheme.len();
        column = column.saturating_sub(cells(grapheme));
    }
    let visible = prefix(&text[start..], width);
    (visible, column)
}

#[cfg(test)]
mod tests {
    use super::*;

    const FAMILY: &str = "👨\u{200d}👩\u{200d}👧";
    const THUMB: &str = "👍\u{1f3fd}";
    const HEART: &str = "❤\u{fe0f}";
    const FLAG: &str = "🇫🇷";
    const ACCENT: &str = "e\u{301}";

    #[test]
    fn width_counts_grapheme_clusters_as_the_terminal_paints_them() {
        for (text, expected) in [
            ("", 0),
            ("abc", 3),
            ("你好", 4),
            (ACCENT, 1),
            ("a\u{301}\u{302}", 1),
            (FAMILY, 2),
            (THUMB, 2),
            (HEART, 2),
            (FLAG, 2),
            ("🇫🇷🇩🇪", 4),
            ("\u{200b}", 0),
            ("a\tb", 2),
            ("ｶﾞ", 2),
        ] {
            assert_eq!(width(text), expected, "{text:?}");
        }
    }

    #[test]
    fn prefix_never_splits_a_cluster() {
        for (text, budget, expected) in [
            ("abc", 0, ""),
            ("abc", 2, "ab"),
            ("abc", 9, "abc"),
            ("你好", 1, ""),
            ("你好", 3, "你"),
            (FAMILY, 1, ""),
            (FAMILY, 2, FAMILY),
            ("a👨\u{200d}👩\u{200d}👧b", 2, "a"),
            ("a👨\u{200d}👩\u{200d}👧b", 3, "a👨\u{200d}👩\u{200d}👧"),
            (&format!("{ACCENT}x"), 1, ACCENT),
            (&format!("{FLAG}{FLAG}"), 3, FLAG),
        ] {
            assert_eq!(prefix(text, budget), expected, "{text:?} / {budget}");
        }
    }

    #[test]
    fn fit_pads_or_ellipsizes_to_the_exact_width() {
        for (text, cells, expected) in [
            ("abc", 5, "abc  "),
            ("abc", 3, "abc"),
            ("abcdef", 4, "abc…"),
            ("abcdef", 1, "…"),
            ("abcdef", 0, ""),
            ("你好世界", 5, "你好…"),
            ("你好世界", 4, "你… "),
            ("你好世界", 2, "… "),
            ("你", 1, "…"),
            (&format!("{FAMILY} hello"), 5, &format!("{FAMILY} h…")),
            (&format!("{FAMILY}{FAMILY}"), 3, &format!("{FAMILY}…")),
            (&format!("{FAMILY}{FAMILY}"), 2, "… "),
            (
                &format!("{ACCENT}{ACCENT}{ACCENT}"),
                2,
                &format!("{ACCENT}…"),
            ),
        ] {
            let result = fit(text, cells);
            assert_eq!(result, expected, "{text:?} / {cells}");
            assert_eq!(width(&result), cells, "{text:?} / {cells}");
        }
    }

    #[test]
    fn wrap_keeps_clusters_whole_and_lines_within_the_width() {
        for (text, cells, expected) in [
            ("", 4, vec![""]),
            ("abcdef", 4, vec!["abcd", "ef"]),
            ("abcd", 4, vec!["abcd"]),
            ("你好世", 4, vec!["你好", "世"]),
            ("你好世", 3, vec!["你", "好", "世"]),
            // A cluster wider than the line still gets a line of its own.
            ("你好", 1, vec!["你", "好"]),
            ("abcdef", 0, vec!["a", "b", "c", "d", "e", "f"]),
        ] {
            assert_eq!(wrap(text, cells), expected, "{text:?} / {cells}");
        }
        let text = format!("a{FAMILY}b{FAMILY}{ACCENT}{FLAG}");
        let lines = wrap(&text, 3);
        assert_eq!(lines.concat(), text);
        assert_eq!(
            lines,
            vec![
                format!("a{FAMILY}"),
                format!("b{FAMILY}"),
                format!("{ACCENT}{FLAG}")
            ]
        );
        assert!(lines.iter().all(|line| width(line) <= 3));
    }

    #[test]
    fn wrap_words_breaks_at_spaces_and_hard_wraps_long_words() {
        for (text, cells, expected) in [
            ("", 4, vec![""]),
            ("hello world", 20, vec!["hello world"]),
            ("hello world", 8, vec!["hello", "world"]),
            ("one two three", 7, vec!["one two", "three"]),
            ("one two three", 8, vec!["one two", "three"]),
            ("abcde fgh", 5, vec!["abcde", "fgh"]),
            ("a  b", 2, vec!["a", "b"]),
            ("abcd efgh", 4, vec!["abcd", "efgh"]),
            ("abcdefghij", 4, vec!["abcd", "efgh", "ij"]),
            ("ab cdefghij", 4, vec!["ab", "cdef", "ghij"]),
            ("abc ", 3, vec!["abc"]),
            ("  abc", 5, vec!["abc"]),
            ("你好世界", 4, vec!["你好", "世界"]),
            ("你好 世界 ok", 5, vec!["你好", "世界", "ok"]),
            ("abc", 0, vec!["a", "b", "c"]),
        ] {
            assert_eq!(wrap_words(text, cells), expected, "{text:?} / {cells}");
        }
        let text = format!("{FAMILY} {FAMILY}{FAMILY} {ACCENT}x{FLAG}");
        let lines = wrap_words(&text, 4);
        assert_eq!(
            lines,
            vec![
                FAMILY.to_owned(),
                format!("{FAMILY}{FAMILY}"),
                format!("{ACCENT}x{FLAG}")
            ]
        );
        assert!(lines.iter().all(|line| width(line) <= 4));
    }

    #[test]
    fn boundaries_move_over_whole_clusters_from_any_offset() {
        let text = format!("a{FAMILY}{ACCENT}{FLAG}你");
        let starts = [
            0,
            1,
            1 + FAMILY.len(),
            1 + FAMILY.len() + ACCENT.len(),
            1 + FAMILY.len() + ACCENT.len() + FLAG.len(),
            text.len(),
        ];
        for pair in starts.windows(2) {
            assert_eq!(next_boundary(&text, pair[0]), pair[1]);
            assert_eq!(prev_boundary(&text, pair[1]), pair[0]);
        }
        // A cursor left inside a cluster still resolves to its edges.
        let inside = 1 + '👨'.len_utf8();
        assert_eq!(prev_boundary(&text, inside), 1);
        assert_eq!(next_boundary(&text, inside), 1 + FAMILY.len());
        assert_eq!(prev_boundary(&text, 0), 0);
        assert_eq!(next_boundary(&text, text.len()), text.len());
        assert_eq!(prev_boundary("", 0), 0);
        assert_eq!(next_boundary("", 0), 0);
    }

    #[test]
    fn window_scrolls_by_cells_and_starts_on_a_cluster() {
        assert_eq!(window("abc", 3, 0), ("", 0));
        assert_eq!(window("abc", 3, 10), ("abc", 3));
        assert_eq!(window("abcdef", 6, 4), ("def", 3));
        assert_eq!(window("abcdef", 2, 4), ("abcd", 2));
        assert_eq!(window("你好世界", "你好世界".len(), 5), ("世界", 4));
        let text = format!("{FAMILY}{FAMILY}x");
        let cursor = text.len();
        let (visible, column) = window(&text, cursor, 4);
        assert_eq!(visible, format!("{FAMILY}x"));
        assert_eq!(column, 3);
        let text = format!("ab{ACCENT}cd");
        let cursor = text.len();
        let (visible, column) = window(&text, cursor, 4);
        assert_eq!(visible, format!("{ACCENT}cd"));
        assert_eq!(column, 3);
    }
}
