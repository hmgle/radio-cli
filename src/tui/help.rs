//! Keyboard and command help, laid out from structured locale entries.
//!
//! Each section body in the catalogs holds one entry per line. `keys<TAB>text`
//! is an item (a key or command with its description); `keys<TAB>` with
//! nothing after the tab is a usage-only item; a line without a tab is a note.

use super::text;
use crate::messages::{self, Locale};
use ratatui::{
    style::{Modifier, Style},
    text::{Line, Span},
};

const INDENT: usize = 2;
const GAP: usize = 2;
/// Hanging indent of descriptions stacked under their keys and of wrapped usage.
const HANG: usize = 2;
/// Below this content width the key sits above its description.
const STACK_BELOW: usize = 50;

#[derive(Debug, PartialEq, Eq)]
pub(super) enum Entry {
    Item { keys: String, detail: String },
    Note(String),
}

pub(super) struct Section {
    pub(super) title: &'static str,
    pub(super) entries: Vec<Entry>,
}

fn parse(body: &str) -> Vec<Entry> {
    body.lines()
        .filter(|line| !line.is_empty())
        .map(|line| match line.split_once('\t') {
            Some((keys, detail)) => Entry::Item {
                keys: keys.to_owned(),
                detail: detail.to_owned(),
            },
            None => Entry::Note(line.to_owned()),
        })
        .collect()
}

pub(super) fn sections(locale: Locale, jump_binding: &str) -> Vec<Section> {
    let section = |title: &'static str, body: &'static str| Section {
        title,
        entries: parse(body),
    };
    let mut playback = section(
        messages::text_in(locale, "help_play_title"),
        messages::text_in(locale, "help_play"),
    );
    let binding = crate::clean_text(jump_binding);
    if !binding.is_empty() {
        // Keep the binding with the other keys, ahead of any trailing note.
        let at = playback
            .entries
            .iter()
            .rposition(|entry| matches!(entry, Entry::Item { .. }))
            .map_or(0, |last| last + 1);
        playback.entries.insert(
            at,
            Entry::Item {
                keys: binding,
                detail: messages::text_in(locale, "help_jump").to_owned(),
            },
        );
    }
    let mut notes = section(
        messages::text_in(locale, "help_notes_title"),
        messages::text_in(locale, "help_notes"),
    );
    notes.entries.push(Entry::Note(
        messages::text_in(locale, "catalog_source_notice").to_owned(),
    ));
    vec![
        section(
            messages::text_in(locale, "help_nav_title"),
            messages::text_in(locale, "help_nav"),
        ),
        playback,
        section(
            messages::text_in(locale, "help_find_title"),
            messages::text_in(locale, "help_find"),
        ),
        section(
            messages::text_in(locale, "help_general_title"),
            messages::text_in(locale, "help_general"),
        ),
        section(
            messages::text_in(locale, "help_input_title"),
            messages::text_in(locale, "help_input"),
        ),
        section(
            messages::text_in(locale, "help_commands_title"),
            messages::text_in(locale, "help_commands"),
        ),
        section(
            messages::text_in(locale, "help_mouse_title"),
            messages::text_in(locale, "help_mouse"),
        ),
        section(
            messages::text_in(locale, "help_context_title"),
            messages::text_in(locale, "help_context"),
        ),
        notes,
    ]
}

/// Help laid out for a popup body `width` cells wide.
#[cfg(test)]
pub(super) fn lines(locale: Locale, jump_binding: &str, width: u16) -> Vec<Line<'static>> {
    lines_in(locale, jump_binding, width, crate::config::Theme::Default)
}

pub(super) fn lines_in(
    locale: Locale,
    jump_binding: &str,
    width: u16,
    theme: crate::config::Theme,
) -> Vec<Line<'static>> {
    layout(
        &sections(locale, jump_binding),
        usize::from(width.max(1)),
        theme,
    )
}

fn key_style(theme: crate::config::Theme) -> Style {
    super::styles::style(theme, super::styles::Role::Focus)
}

/// A key or usage string on its own line(s); wrapped rows hang under the first.
fn key_rows(keys: &str, inner: usize, theme: crate::config::Theme) -> Vec<Line<'static>> {
    text::wrap_words(keys, inner.saturating_sub(HANG))
        .into_iter()
        .enumerate()
        .map(|(row, part)| {
            Line::from(vec![
                Span::raw(" ".repeat(INDENT + if row == 0 { 0 } else { HANG })),
                Span::styled(part.to_owned(), key_style(theme)),
            ])
        })
        .collect()
}

fn layout(sections: &[Section], width: usize, theme: crate::config::Theme) -> Vec<Line<'static>> {
    let inner = width.saturating_sub(INDENT).max(1);
    // Keys wider than this are stacked above their description even on wide screens.
    let cap = if inner >= STACK_BELOW {
        inner * 2 / 5
    } else {
        0
    };
    let heading = Style::default().add_modifier(Modifier::BOLD);
    let key_style = key_style(theme);
    let indent = |cells: usize| Span::raw(" ".repeat(cells));
    let mut out = Vec::new();
    for (index, section) in sections.iter().enumerate() {
        if index > 0 {
            out.push(Line::default());
        }
        for part in text::wrap_words(section.title, width) {
            out.push(Line::from(Span::styled(part.to_owned(), heading)));
        }
        let column = section
            .entries
            .iter()
            .filter_map(|entry| match entry {
                Entry::Item { keys, detail } if !detail.is_empty() => Some(text::width(keys)),
                _ => None,
            })
            .filter(|keys| *keys <= cap)
            .max()
            .unwrap_or(0);
        for entry in &section.entries {
            match entry {
                Entry::Note(note) => {
                    for part in text::wrap_words(note, inner) {
                        out.push(Line::from(vec![indent(INDENT), Span::raw(part.to_owned())]));
                    }
                }
                Entry::Item { keys, detail } if detail.is_empty() => {
                    out.extend(key_rows(keys, inner, theme));
                }
                Entry::Item { keys, detail } if column > 0 && text::width(keys) <= column => {
                    let parts = text::wrap_words(detail, inner - column - GAP);
                    for (row, part) in parts.into_iter().enumerate() {
                        let lead = if row == 0 {
                            vec![
                                indent(INDENT),
                                Span::styled(keys.clone(), key_style),
                                indent(column - text::width(keys) + GAP),
                            ]
                        } else {
                            vec![indent(INDENT + column + GAP)]
                        };
                        let mut spans = lead;
                        spans.push(Span::raw(part.to_owned()));
                        out.push(Line::from(spans));
                    }
                }
                Entry::Item { keys, detail } => {
                    let rows = key_rows(keys, inner, theme);
                    // Below a wrapped key, step in once more so the description stays distinct.
                    let hang = if rows.len() > 1 { 2 * HANG } else { HANG };
                    out.extend(rows);
                    for part in text::wrap_words(detail, inner.saturating_sub(hang)) {
                        out.push(Line::from(vec![
                            indent(INDENT + hang),
                            Span::raw(part.to_owned()),
                        ]));
                    }
                }
            }
        }
    }
    out
}
