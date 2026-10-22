use super::{
    state::{App, Focus, Mode, ScrollExtent, Source},
    worker::{PageStatus, View},
};
use crate::model::Station;
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, List, ListItem, Paragraph, Wrap},
};
use unicode_width::UnicodeWidthStr;

fn panel(title: String, active: bool) -> Block<'static> {
    Block::default()
        .title(if active {
            format!(" ▶ {title} ")
        } else {
            format!(" {title} ")
        })
        .borders(Borders::ALL)
        .border_type(if active {
            BorderType::Thick
        } else {
            BorderType::Rounded
        })
        .border_style(if active {
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::DarkGray)
        })
        .title_style(if active {
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::Gray)
        })
}
fn bitrate(station: &Station) -> String {
    if station.bitrate == 0 {
        crate::t!("bitrate_unknown").into()
    } else {
        format!("{} kbps", station.bitrate)
    }
}
fn codec(station: &Station) -> String {
    if station.codec.is_empty() || station.codec.eq_ignore_ascii_case("unknown") {
        crate::t!("codec_unknown").into()
    } else {
        crate::clean_text(&station.codec)
    }
}
fn value(text: &str) -> String {
    if text.trim().is_empty() {
        crate::t!("value_unknown").into()
    } else {
        crate::clean_text(text)
    }
}

pub(super) fn fit(text: &str, width: usize) -> String {
    let text = crate::clean_text(text);
    fit_clean(&text, width)
}

/// Callers must have already sanitized external text.
fn fit_clean(text: &str, width: usize) -> String {
    super::text::fit(text, width)
}

pub(super) fn draw(frame: &mut Frame, app: &mut App, view: &View) {
    let image = draw_content(frame, app, view);
    let theme = app.theme;
    if theme == crate::config::Theme::Default {
        return;
    }
    let width = usize::from(frame.area().width);
    for (index, cell) in frame.buffer_mut().content.iter_mut().enumerate() {
        // Artwork colors are picture data, not interface colors.
        if let Some(image) = image
            && image.contains(ratatui::layout::Position::new(
                (index % width) as u16,
                (index / width) as u16,
            ))
        {
            continue;
        }
        if theme == crate::config::Theme::Mono {
            cell.fg = Color::Reset;
            cell.bg = Color::Reset;
        } else {
            let selected = cell.bg == Color::Cyan;
            cell.fg = if selected {
                Color::White
            } else {
                match cell.fg {
                    Color::Cyan => Color::Blue,
                    Color::Yellow => Color::Magenta,
                    Color::Gray | Color::DarkGray | Color::White | Color::Reset => Color::Black,
                    color => color,
                }
            };
            cell.bg = if selected { Color::Blue } else { Color::White };
        }
    }
}

struct ScreenLayout {
    rects: [Rect; 5],
    bottom: Rect,
    shared: bool,
    task_main: bool,
}
fn screen_layout(area: Rect, focus: Focus, task_active: bool) -> ScreenLayout {
    let mut rects = [Rect::default(); 5];
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(if area.width >= 100 && area.height >= 20 {
                5
            } else {
                4
            }),
            Constraint::Min(3),
            Constraint::Length(
                if area.height >= 20 && (task_active || focus == Focus::Activity) {
                    4
                } else {
                    1
                },
            ),
            Constraint::Length(3),
        ])
        .split(area);
    rects[Focus::Player.index()] = vertical[0];
    rects[Focus::Activity.index()] = vertical[2];
    if area.width >= 100 && area.height >= 20 {
        let columns = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Length(18),
                Constraint::Percentage(48),
                Constraint::Min(25),
            ])
            .split(vertical[1]);
        rects[Focus::Sources.index()] = columns[0];
        rects[Focus::List.index()] = columns[1];
        rects[Focus::Details.index()] = columns[2];
    } else {
        let sources = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(3), Constraint::Min(0)])
            .split(vertical[1]);
        rects[Focus::Sources.index()] = sources[0];
        // Narrow screens devote the content region to the list; 3 opens details.
        rects[Focus::List.index()] = sources[1];
        rects[Focus::Details.index()] = sources[1];
    }
    let task_main = area.height < 20 && focus == Focus::Activity;
    if task_main {
        rects[Focus::Activity.index()] = rects[Focus::List.index()];
        rects[Focus::List.index()] = Rect::default();
        rects[Focus::Details.index()] = Rect::default();
    }
    ScreenLayout {
        rects,
        bottom: vertical[3],
        shared: area.width < 100 || area.height < 20,
        task_main,
    }
}

fn station_row(
    station: &Station,
    marker: Option<(&'static str, Style)>,
    width: u16,
) -> Line<'static> {
    let width = usize::from(width);
    let suffix = if station.active {
        ""
    } else {
        crate::t!("station_inactive_short")
    };
    let mut available = width.saturating_sub(3 + suffix.width());
    let mut metadata = String::new();
    for field in [
        format!(" [{}]", fit(&station.countrycode, 2)),
        format!(" {}", bitrate(station)),
        format!(" {}", codec(station)),
    ] {
        if available >= 16 + field.width() {
            available -= field.width();
            metadata.push_str(&field);
        }
    }
    let (glyph, style) = marker.unwrap_or((" ", Style::default()));
    Line::from(vec![
        Span::styled(glyph, style),
        Span::raw(format!(
            "{} {}{}{}",
            if station.favorite { "★" } else { " " },
            fit(&station.name, available),
            metadata,
            suffix
        )),
    ])
}

/// Returns the area covered by artwork, if any was painted.
fn draw_content(frame: &mut Frame, app: &mut App, view: &View) -> Option<Rect> {
    let mut painted = None;
    let area = frame.area();
    app.source_rows = [Rect::default(); 4];
    app.filter_summary = Rect::default();
    app.update_size(area.width, area.height);
    if app.too_small {
        frame.render_widget(
            Paragraph::new(
                crate::t!("terminal_too_small", "width" => area.width, "height" => area.height),
            ),
            area,
        );
        return None;
    }
    let layout = screen_layout(area, app.focus, view.task_active);
    app.set_rects(layout.rects);
    let normal = app.mode == Mode::Normal
        && !app.help
        && app.filter_edit.is_none()
        && app.confirmation.is_none();
    let active = |focus| normal && app.focus == focus;
    let detail_width = (usize::from(area.width) / 4).clamp(12, 40);
    let playing = {
        let state = &view.status.playback_state;
        let status = super::display::playback_status(state, detail_width);
        if matches!(state, crate::engine::PlaybackState::Failed { .. }) {
            format!("{status} · ?")
        } else {
            status
        }
    };
    let recording = super::display::recording_header(&view.status, detail_width);
    let volume = format!(
        "{}%{}",
        view.status.volume,
        if view.status.muted {
            crate::t!("player_muted")
        } else {
            ""
        }
    );
    let player_text = if let Some(station) = &view.status.station {
        crate::t!(
            "player_station",
            "state" => playing,
            "name" => fit_clean(
                &value(&station.name),
                usize::from(area.width.saturating_sub(40))
            )
            .trim_end(),
            "volume" => volume,
            "recording" => recording,
            "codec" => codec(station),
            "bitrate" => bitrate(station),
            "country" => value(&station.countrycode),
            "language" => value(&station.language),
            "title" => if view.status.title.is_empty() {
                crate::t!("player_no_title").into()
            } else {
                value(&view.status.title)
            }
        )
    } else {
        crate::t!("player_idle_panel", "volume" => volume, "recording" => recording)
    };
    let player_text = if layout.shared {
        let width = usize::from(area.width.saturating_sub(2));
        let state = format!(" · {playing}");
        let volume = format!(" · {volume}");
        let name = view
            .status
            .station
            .as_ref()
            .map(|s| value(&s.name))
            .unwrap_or_else(|| crate::t!("player_idle").into());
        let volume = if width >= 60 { volume } else { String::new() };
        let first = format!(
            "{}{}{}",
            fit(&name, width.saturating_sub(state.width() + volume.width())).trim_end(),
            state,
            volume
        );
        let recording = if width < 60
            && matches!(
                view.status.recording_state,
                crate::engine::RecordingState::Idle
            ) {
            if view.status.muted {
                crate::t!("player_muted").into()
            } else {
                String::new()
            }
        } else {
            format!(
                " · {}",
                fit(&recording, width.saturating_sub(19)).trim_end()
            )
        };
        let title = if view.status.title.is_empty() {
            crate::t!("player_no_title").into()
        } else {
            value(&view.status.title)
        };
        format!(
            "{first}\n{}{}",
            fit(&title, width.saturating_sub(recording.width())).trim_end(),
            recording
        )
    } else {
        player_text
    };
    frame.render_widget(
        Paragraph::new(player_text).block(panel(
            crate::t!("player_panel").into(),
            active(Focus::Player),
        )),
        app.rects[Focus::Player.index()],
    );

    let source_rect = app.rects[Focus::Sources.index()];
    frame.render_widget(
        panel(crate::t!("sources_panel").into(), active(Focus::Sources)),
        source_rect,
    );
    let inner = source_rect.inner(ratatui::layout::Margin::new(1, 1));
    if source_rect.height >= 6 {
        for source in Source::ALL {
            let rect = Rect::new(inner.x, inner.y + source.index() as u16, inner.width, 1);
            app.source_rows[source.index()] = rect;
            frame.render_widget(
                Paragraph::new(format!(
                    "{} {}",
                    if app.source == source { "›" } else { " " },
                    source.label()
                ))
                .style(if app.source == source {
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                }),
                rect,
            );
        }
        if inner.height > 6 {
            frame.render_widget(
                Paragraph::new(crate::t!("sources_keys"))
                    .style(Style::default().fg(Color::DarkGray)),
                Rect::new(inner.x, inner.y + 6, inner.width, inner.height - 6),
            );
        }
    } else {
        let columns = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Ratio(1, 4); 4])
            .split(inner);
        for source in Source::ALL {
            let rect = columns[source.index()];
            app.source_rows[source.index()] = rect;
            frame.render_widget(
                Paragraph::new(format!(
                    "{}{}",
                    if app.source == source { ">" } else { " " },
                    source.short_label()
                ))
                .style(if app.source == source {
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                }),
                rect,
            );
        }
    }
    let show_details = !layout.task_main && (!layout.shared || app.focus == Focus::Details);
    if !layout.task_main && (!layout.shared || app.focus != Focus::Details) {
        let title = crate::t!(
            "list_title",
            "prefix" => if layout.shared { crate::t!("list_shared") } else { "2" },
            "source" => app.source.label(),
            "count" => app.length(view),
            "total" => if app.source == Source::Recordings {
                view.recordings_total
            } else {
                view.total
            },
            "page" => app.filter.offset / app.filter.limit.max(1) + 1
        );
        let items: Vec<ListItem> = if app.source == Source::Recordings {
            view.recordings_for(&app.filter)
                .iter()
                .map(|r| {
                    let width = usize::from(app.rects[Focus::List.index()].width.saturating_sub(4));
                    let status = format!(
                        " · {}{}",
                        super::display::recording_status(&r.status),
                        if r.reconnects > 0 {
                            format!(" · {}", crate::t!("recording_gap_marker"))
                        } else {
                            String::new()
                        }
                    );
                    let path = if width >= 70 {
                        format!(" · {}", fit(&r.path, 24).trim_end())
                    } else {
                        String::new()
                    };
                    let name = fit(
                        &r.station_name,
                        width.saturating_sub(status.width() + path.width()),
                    );
                    ListItem::new(format!("{name}{status}{path}"))
                })
                .collect()
        } else {
            let state = &view.status.playback_state;
            let marker = super::display::playback_glyph(state)
                .map(|glyph| (glyph, super::display::playback_tone(state).style()));
            view.stations_for(&app.filter)
                .iter()
                .map(|s| {
                    let current = view
                        .status
                        .station
                        .as_ref()
                        .is_some_and(|now| now.stationuuid == s.stationuuid);
                    ListItem::new(station_row(
                        s,
                        marker.filter(|_| current),
                        app.rects[Focus::List.index()].width.saturating_sub(4),
                    ))
                })
                .collect()
        };
        let rect = app.rects[Focus::List.index()];
        if items.is_empty() {
            frame.render_widget(
                Paragraph::new(
                    match view.page_status(&app.filter) {
                        PageStatus::Failed(error) => crate::t!("page_failed", "detail" => fit(error, usize::from(rect.width.saturating_sub(2))).trim_end()),
                        PageStatus::Loading => crate::t!("page_loading").into(),
                        PageStatus::Ready => empty_hint(app).into(),
                    },
                )
                .wrap(Wrap { trim: true })
                .block(panel(title, active(Focus::List))),
                rect,
            );
        } else {
            frame.render_stateful_widget(
                List::new(items)
                    .block(panel(title, active(Focus::List)))
                    .highlight_style(if active(Focus::List) {
                        Style::default()
                            .bg(Color::Cyan)
                            .fg(Color::Black)
                            .add_modifier(Modifier::BOLD)
                    } else {
                        Style::default()
                            .fg(Color::Gray)
                            .add_modifier(Modifier::BOLD)
                    })
                    .highlight_symbol(if active(Focus::List) { "› " } else { "· " }),
                rect,
                &mut app.list,
            );
        }
    }
    if show_details {
        let text = if app.source == Source::Recordings && !app.inspect_info {
            view.recordings_for(&app.filter).get(app.list.selected().unwrap_or(0)).map(|r| {
                crate::t!(
                    "recording_details",
                    "name" => value(&r.station_name),
                    "status" => super::display::recording_status(&r.status),
                    "codec" => r.codec,
                    "container" => r.container,
                    "duration" => r.media_duration_ms
                        .map(super::display::duration)
                        .unwrap_or_else(|| crate::t!("value_unknown").into()),
                    "size" => super::display::size(r.size_bytes),
                    "started" => r.started_at,
                    "stopped" => super::display::recording_stop_time(&r.status, r.stopped_at.as_deref()),
                    "uuid" => r.stationuuid,
                    "url" => crate::clean_text(&r.source_url_resolved),
                    "path" => crate::clean_text(&r.path),
                    "reconnects" => if r.reconnects > 0 {
                        format!("{}\n", crate::t!("recording_reconnected", "count" => r.reconnects))
                    } else {
                        String::new()
                    },
                    "error" => r.error.as_deref().map(|error| if app.recording_error_expanded {
                        error.lines().map(crate::clean_text).collect::<Vec<_>>().join("\n")
                    } else {
                        crate::t!("recording_expand_error", "detail" => crate::clean_text(error.lines().next().unwrap_or("")))
                    }).unwrap_or_default()
                )
            }).unwrap_or_else(|| crate::t!("no_recording_records").into())
        } else if app.inspect_info && view.inspected_request != app.info_request {
            crate::t!("details_loading").into()
        } else if app.inspect_info && view.inspected_error.is_some() {
            // scroll_text sanitizes each line while preserving long diagnostics.
            view.inspected_error.clone().unwrap()
        } else if let Some(station) = if app.inspect_info {
            view.inspected.as_ref()
        } else {
            app.selected(view)
        } {
            station_details(station, view)
        } else {
            crate::t!("details_empty").into()
        };
        let image =
            super::artwork::image(app, view, area).map(|(image, rect)| (image.clone(), rect));
        let text_rect = app.rects[Focus::Details.index()];
        let block = panel(
            if layout.shared {
                crate::t!("details_shared")
            } else {
                crate::t!("details_panel")
            }
            .into(),
            active(Focus::Details),
        );
        frame.render_widget(
            scroll_text(&text, text_rect, &mut app.details_scroll)
                .0
                .block(block),
            text_rect,
        );
        if let Some((image, rect)) = image {
            painted = Some(super::artwork::render(frame, &image, rect));
        }
    }
    let task = if view.task.is_empty() {
        view.catalog_note.clone()
    } else {
        format!("{} · {}", view.task, view.catalog_note)
    };
    let rect = app.rects[Focus::Activity.index()];
    let visible = usize::from(rect.height.saturating_sub(3));
    app.activity_scroll = app
        .activity_scroll
        .min(view.logs.len().saturating_sub(visible) as u16);
    let end = view
        .logs
        .len()
        .saturating_sub(usize::from(app.activity_scroll));
    let start = end.saturating_sub(visible);
    let mut logs = vec![Line::from(Span::styled(
        crate::clean_text(&task),
        Style::default().fg(if view.task_active {
            Color::Yellow
        } else {
            Color::Gray
        }),
    ))];
    logs.extend(
        view.logs
            .iter()
            .skip(start)
            .take(end - start)
            .map(|line| Line::from(line.as_ref())),
    );
    if rect.height == 1 {
        frame.render_widget(
            Paragraph::new(format!(
                "5 {} · {}",
                crate::clean_text(&task),
                crate::clean_text(&view.status.message)
            )),
            rect,
        );
    } else {
        frame.render_widget(
            Paragraph::new(logs).block(panel(
                crate::t!("activity_panel").into(),
                active(Focus::Activity),
            )),
            rect,
        );
    }

    let bottom = layout.bottom;
    if app.mode != Mode::Normal {
        let label = if app.mode == Mode::Command {
            crate::t!("mode_command")
        } else {
            crate::t!("mode_search")
        };
        let input = Rect::new(bottom.x, bottom.y, bottom.width, 1);
        let (input_width, cursor_start) = input_geometry(label, input.width);
        let (text, cursor) = input_window(&app.editor.text, app.editor.cursor, input_width);
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(
                    format!("{label} "),
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(text),
            ])),
            input,
        );
        frame.set_cursor_position((input.x + cursor_start + cursor, input.y));
        let hint = if app.message.is_empty() {
            if app.mode == Mode::Command {
                crate::t!("command_keys")
            } else {
                crate::t!("search_keys")
            }
        } else {
            &app.message
        };
        frame.render_widget(
            Paragraph::new(crate::clean_text(hint))
                .style(Style::default().fg(Color::Yellow))
                .wrap(Wrap { trim: true }),
            Rect::new(bottom.x, bottom.y + 1, bottom.width, 2),
        );
    } else {
        let filter = if app.source == Source::Recordings {
            crate::t!("recordings_no_filters").into()
        } else {
            crate::clean_text(&format!(
                "/{}  {} {} {} ≥{}k",
                crate::clean_text(&app.filter.query),
                app.filter
                    .country
                    .as_deref()
                    .unwrap_or(crate::t!("all_countries")),
                app.filter
                    .language
                    .as_deref()
                    .unwrap_or(crate::t!("all_languages")),
                app.filter.tag.as_deref().unwrap_or(crate::t!("all_tags")),
                app.filter.min_bitrate
            ))
        };
        let context = context_hint(app.source, app.focus);
        // A modal owns the keyboard, so the bar names it instead of the panel behind it.
        let (mode_label, modal_keys) = if app.confirmation.is_some() {
            (
                crate::t!("mode_confirm").to_owned(),
                Some(crate::t!("confirm_keys").to_owned()),
            )
        } else if app.help {
            (
                crate::t!("mode_help").to_owned(),
                Some(
                    if app.help_content.is_some() {
                        crate::t!("help_keys_more")
                    } else {
                        crate::t!("help_keys")
                    }
                    .to_owned(),
                ),
            )
        } else if app.filter_edit.is_some() {
            (
                crate::t!("mode_filters").to_owned(),
                Some(format!(
                    "{} · {}",
                    crate::t!("filter_keys_edit"),
                    crate::t!("filter_keys_apply")
                )),
            )
        } else {
            (
                format!("{} · {}", crate::t!("mode_normal"), app.focus.label()),
                None,
            )
        };
        let badge = format!(" {mode_label} ");
        let clear_width = if app.source == Source::Recordings {
            0
        } else {
            crate::t!("clear_filters_label").width()
        };
        let filter = fit(
            &filter,
            usize::from(bottom.width).saturating_sub(badge.width() + clear_width),
        );
        frame.render_widget(
            Paragraph::new(vec![
                Line::from(vec![
                    Span::styled(
                        badge,
                        Style::default().bg(Color::Cyan).fg(Color::Black),
                    ),
                    Span::raw(filter),
                ]),
                Line::from(if view.operation_error_id > app.dismissed_error {
                    crate::t!("operation_failed", "detail" => fit(&view.operation_error, usize::from(bottom.width).saturating_sub(crate::t!("operation_failed", "detail" => "").width())).trim_end())
                } else if app.message.is_empty() {
                    context.into()
                } else {
                    app.message.clone()
                }),
                Line::from(if let Some(keys) = modal_keys {
                    keys
                } else if bottom.width < 80 {
                    crate::t!("keys_compact").into()
                } else if app.source == Source::Recordings {
                    crate::t!("keys_recordings").into()
                } else {
                    crate::t!("keys_global").into()
                }),
            ]),
            bottom,
        );
    }
    if app.mode == Mode::Normal && app.source != Source::Recordings {
        let label = crate::t!("clear_filters_label");
        let width = label.width() as u16;
        if width <= bottom.width / 2 {
            let rect = Rect::new(bottom.right() - width, bottom.y, width, 1);
            if app.confirmation.is_none() && !app.help && app.filter_edit.is_none() {
                app.filter_summary = rect;
            }
            frame.render_widget(Paragraph::new(label), rect);
        }
    }
    if let Some((selected, values)) = &app.filter_edit {
        let rect = popup(area, 72, 18);
        clear_popup(frame, rect);
        let names = [
            crate::t!("filter_country"),
            crate::t!("filter_language"),
            crate::t!("filter_tag"),
            crate::t!("filter_bitrate"),
        ];
        let label_width = format!("› {}: ", names[*selected]).width() as u16;
        let input_width = rect.width.saturating_sub(2 + label_width);
        let mut text = names
            .iter()
            .enumerate()
            .map(|(i, name)| {
                Line::from(Span::styled(
                    format!(
                        "{} {name}: {}",
                        if i == *selected { "›" } else { " " },
                        if i == *selected {
                            input_window(
                                &values[i],
                                app.filter_input.cursor.min(values[i].len()),
                                input_width,
                            )
                            .0
                        } else {
                            values[i].clone()
                        }
                    ),
                    if i == *selected {
                        Style::default().fg(Color::Yellow)
                    } else {
                        Style::default()
                    },
                ))
            })
            .chain(if rect.width >= 60 {
                vec![
                    Line::from(format!(
                        "{} · {}",
                        crate::t!("filter_keys_edit"),
                        crate::t!("filter_keys_apply")
                    )),
                    Line::from(crate::t!("filter_keys_clear")),
                ]
            } else {
                vec![
                    Line::from(crate::t!("filter_keys_edit")),
                    Line::from(crate::t!("filter_keys_apply")),
                    Line::from(crate::t!("filter_keys_clear")),
                ]
            })
            .chain((!app.message.is_empty()).then(|| Line::from(crate::clean_text(&app.message))))
            .collect::<Vec<_>>();
        if *selected < 3 {
            let candidates = app.facet_candidates(view, *selected);
            let reserved = text.len() as u16 + 3 + u16::from(view.facets_error.is_some());
            let count = usize::from(rect.height.saturating_sub(reserved)).min(8);
            let start = app
                .facet_index
                .unwrap_or(0)
                .saturating_sub(count / 2)
                .min(candidates.len().saturating_sub(count));
            text.push(Line::from(if view.facets_loading {
                crate::t!("facets_loading")
            } else {
                crate::t!("facets_hint")
            }));
            if let Some(error) = &view.facets_error {
                text.push(Line::from(crate::clean_text(error)));
            }
            text.extend(candidates.iter().enumerate().skip(start).take(count).map(
                |(index, facet)| {
                    Line::from(format!(
                        "{} {} ({})",
                        if app.facet_index == Some(index) {
                            "›"
                        } else {
                            " "
                        },
                        crate::clean_text(&if *selected == 0
                            && crate::messages::locale() == crate::messages::Locale::ZhCn
                        {
                            crate::territories::display_zh(&facet.value)
                        } else {
                            facet.value.clone()
                        }),
                        facet.count
                    ))
                },
            ));
        }
        frame.render_widget(
            Paragraph::new(text).block(panel(crate::t!("filter_title").into(), true)),
            rect,
        );
        let (_, cursor) = input_window(
            &values[*selected],
            app.filter_input.cursor.min(values[*selected].len()),
            input_width,
        );
        frame.set_cursor_position((
            rect.x + 1 + label_width + cursor,
            rect.y + 1 + *selected as u16,
        ));
    }
    if app.help {
        let rect = popup(area, 88, area.height.saturating_sub(2));
        clear_popup(frame, rect);
        let inner = rect.width.saturating_sub(2);
        let lines = match &app.help_content {
            Some(content) => wrapped_lines(content, inner),
            None => super::help::lines(crate::messages::locale(), &app.jump_binding, inner),
        };
        let (paragraph, extent) = scroll_lines(lines, rect, &mut app.help_scroll);
        app.popup = extent;
        let title = app.help_title.unwrap_or(crate::t!("help_title"));
        let mut block = panel(title.into(), true);
        if let Some((first, last)) = extent.span(app.help_scroll) {
            let position = format!(" {first}-{last}/{} ", extent.total);
            // The title and a border cell on each side come first; drop the
            // position rather than overlap them on narrow terminals.
            if title.width() + 4 + position.width() + 2 <= usize::from(rect.width) {
                block = block.title(Line::from(position).right_aligned());
            }
        }
        frame.render_widget(paragraph.block(block), rect);
    }
    if let Some(confirmation) = &app.confirmation {
        let rect = popup(area, 72, area.height.saturating_sub(2));
        clear_popup(frame, rect);
        let inner = rect.inner(ratatui::layout::Margin::new(1, 1));
        frame.render_widget(panel(crate::t!("confirm_delete").into(), true), rect);
        let body = format!(
            "{}\n{}\n{}\n{}",
            fit(&confirmation.summary, usize::from(inner.width)).trim_end(),
            crate::clean_text(&confirmation.id),
            confirmation.scope,
            crate::clean_text(&confirmation.error)
        );
        frame.render_widget(
            Paragraph::new(body).wrap(Wrap { trim: false }),
            Rect::new(
                inner.x,
                inner.y,
                inner.width,
                inner.height.saturating_sub(3),
            ),
        );
        frame.render_widget(
            Paragraph::new(crate::t!("confirm_keys")).wrap(Wrap { trim: true }),
            Rect::new(inner.x, inner.bottom().saturating_sub(3), inner.width, 3),
        );
    }
    painted
}

fn empty_hint(app: &App) -> &'static str {
    let filtered = !app.filter.query.is_empty()
        || app.filter.country.is_some()
        || app.filter.language.is_some()
        || app.filter.tag.is_some()
        || app.filter.min_bitrate > 0;
    match app.source {
        Source::Recordings => crate::t!("empty_recordings"),
        _ if filtered => crate::t!("empty_filtered"),
        Source::Stations => crate::t!("empty_stations"),
        Source::Favorites => crate::t!("empty_favorites"),
        Source::History => crate::t!("empty_history"),
    }
}

pub(super) fn station_details(station: &Station, view: &View) -> String {
    let current = view
        .status
        .station
        .as_ref()
        .filter(|s| s.stationuuid == station.stationuuid);
    crate::t!(
        "station_details",
        "name" => value(&station.name),
        "context" => match current {
            Some(_) if view.status.playback_state != crate::engine::PlaybackState::Idle => {
                format!(
                    " · {}",
                    super::display::playback_status(&view.status.playback_state, 0)
                )
            }
            _ => crate::t!("station_selected").into(),
        },
        "country" => value(&crate::territories::display_in(crate::messages::locale(), &station.countrycode, &station.country)),
        "language" => value(&station.language),
        "subdivision" => value(&station.state),
        "bitrate" => bitrate(station),
        "codec" => codec(station),
        "hls" => if station.hls != 0 { crate::t!("value_yes") } else { crate::t!("value_no") },
        "tags" => value(&station.tags),
        "favorite" => if station.favorite { crate::t!("value_yes") } else { crate::t!("value_no") },
        "catalog" => if station.custom {
            crate::t!("station_custom")
        } else if station.active {
            crate::t!("station_active")
        } else {
            crate::t!("station_inactive")
        },
        "uuid" => station.stationuuid,
        "url" => crate::clean_text(station.primary_url()),
        "backups" => if let Some(error) = view.backups_errors.get(&station.stationuuid) {
            crate::clean_text(error)
        } else {
            view.backups
                .get(&station.stationuuid)
                .map(|urls| {
                    urls.iter()
                        .map(|u| crate::clean_text(u))
                        .collect::<Vec<_>>()
                        .join("\n")
                })
                .unwrap_or_default()
        },
        "request" => if current.is_some() {
            crate::t!("current_request_url", "url" => crate::clean_text(&view.status.source_url))
        } else {
            String::new()
        },
        "homepage" => value(&station.homepage)
    )
}

/// Hard-wrap by terminal cells so long URLs remain readable and scrolling cannot
/// move beyond the last page. Lines break only between grapheme clusters.
pub(super) fn wrapped_lines(text: &str, width: u16) -> Vec<Line<'static>> {
    let width = usize::from(width.max(1));
    let mut lines = Vec::new();
    for line in text.lines() {
        let line = crate::clean_text(line);
        lines.extend(
            super::text::wrap(&line, width)
                .into_iter()
                .map(|part| Line::from(part.to_owned())),
        );
    }
    lines
}

fn scroll_text<'a>(text: &'a str, rect: Rect, scroll: &mut u16) -> (Paragraph<'a>, ScrollExtent) {
    scroll_lines(
        wrapped_lines(text, rect.width.saturating_sub(2)),
        rect,
        scroll,
    )
}

/// Scroll already wrapped lines; `rect` is the bordered block they appear in.
fn scroll_lines<'a>(
    lines: Vec<Line<'a>>,
    rect: Rect,
    scroll: &mut u16,
) -> (Paragraph<'a>, ScrollExtent) {
    let extent = ScrollExtent {
        total: lines.len(),
        visible: usize::from(rect.height.saturating_sub(2)),
    };
    *scroll = (*scroll).min(extent.max().min(usize::from(u16::MAX)) as u16);
    (Paragraph::new(lines).scroll((*scroll, 0)), extent)
}
fn popup(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width.saturating_sub(2));
    let height = height.min(area.height.saturating_sub(2));
    Rect::new(
        area.x + (area.width - width) / 2,
        area.y + (area.height - height) / 2,
        width,
        height,
    )
}

/// Render `Clear` for a popup plus the column immediately to its left.
///
/// Ratatui's buffer diff skips the trailing cell of an unchanged wide
/// character, so a left border drawn on that cell would never reach the
/// terminal. Clearing the adjacent column resets any wide character there and
/// lets the border through. The right border needs no such treatment: its left
/// neighbour always lies inside the cleared popup.
fn clear_popup(frame: &mut Frame, rect: Rect) {
    frame.render_widget(
        Clear,
        Rect::new(
            rect.x.saturating_sub(1),
            rect.y,
            rect.width.saturating_add(1),
            rect.height,
        ),
    );
}

pub(super) fn context_hint(source: Source, focus: Focus) -> &'static str {
    match (source, focus) {
        (_, Focus::Sources) => crate::t!("context_sources"),
        (Source::Recordings, Focus::List) => {
            crate::t!("context_recordings")
        }
        (_, Focus::List) => crate::t!("context_stations"),
        (_, Focus::Details) => crate::t!("context_details"),
        (_, Focus::Player) => crate::t!("context_player"),
        (_, Focus::Activity) => crate::t!("context_activity"),
    }
}

pub(super) fn input_geometry(label: &str, width: u16) -> (u16, u16) {
    let start = label.width().saturating_add(1).min(usize::from(width)) as u16;
    (
        width.saturating_sub(start),
        start.min(width.saturating_sub(1)),
    )
}

/// Horizontally scroll input by display cells while keeping the cursor visible.
fn input_window(text: &str, cursor: usize, width: u16) -> (String, u16) {
    let (visible, column) = super::text::window(text, cursor, usize::from(width));
    (visible.to_owned(), column as u16)
}
