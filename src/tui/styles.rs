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
    Shortcut,
}

pub(super) fn base(theme: Theme) -> Style {
    if theme == Theme::Light {
        Style::default().fg(Color::Black).bg(Color::White)
    } else {
        Style::default().fg(Color::Reset).bg(Color::Reset)
    }
}

pub(super) fn style(theme: Theme, role: Role) -> Style {
    if theme == Theme::Mono {
        return base(theme);
    }
    if theme == Theme::Light {
        return match role {
            Role::Focus => Style::default().fg(Color::Blue),
            Role::Border | Role::Muted => Style::default().fg(Color::Black),
            Role::Selected => Style::default().bg(Color::Blue).fg(Color::White),
            Role::Warning => Style::default().fg(Color::Magenta),
            Role::Active => Style::default().fg(Color::Green),
            Role::Error => Style::default().fg(Color::Red),
            Role::Shortcut => Style::default().fg(Color::Blue),
        };
    }
    match role {
        Role::Focus => Style::default().fg(Color::Cyan),
        Role::Border => Style::default().fg(Color::DarkGray),
        Role::Muted => Style::default().fg(Color::Gray),
        Role::Selected => Style::default().bg(Color::Cyan).fg(Color::Black),
        Role::Warning => Style::default().fg(Color::Yellow),
        Role::Active => Style::default().fg(Color::Green),
        Role::Error => Style::default().fg(Color::Red),
        Role::Shortcut => Style::default().fg(Color::LightBlue),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roles_resolve_before_painting_and_mono_never_adds_color() {
        for role in [
            Role::Focus,
            Role::Border,
            Role::Muted,
            Role::Selected,
            Role::Warning,
            Role::Active,
            Role::Error,
            Role::Shortcut,
        ] {
            let mono = style(Theme::Mono, role);
            assert_eq!(mono.fg, Some(Color::Reset));
            assert_eq!(mono.bg, Some(Color::Reset));
            let light = base(Theme::Light).patch(style(Theme::Light, role));
            assert_ne!(light.fg, light.bg);
        }
        assert_eq!(style(Theme::Light, Role::Selected).bg, Some(Color::Blue));
        assert_eq!(style(Theme::Default, Role::Focus).fg, Some(Color::Cyan));
    }
}
