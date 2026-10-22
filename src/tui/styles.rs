//! Styles named by their interface role, independent of widget layout.

use crate::config::Theme;
use ratatui::style::{Color, Style};

#[derive(Clone, Copy)]
pub(super) enum Role {
    Focus,
    Border,
    Muted,
    Selected,
    Warning,
    Active,
    Error,
}

pub(super) fn style(_theme: Theme, role: Role) -> Style {
    match role {
        Role::Focus => Style::default().fg(Color::Cyan),
        Role::Border => Style::default().fg(Color::DarkGray),
        Role::Muted => Style::default().fg(Color::Gray),
        Role::Selected => Style::default().bg(Color::Cyan).fg(Color::Black),
        Role::Warning => Style::default().fg(Color::Yellow),
        Role::Active => Style::default().fg(Color::Green),
        Role::Error => Style::default().fg(Color::Red),
    }
}
