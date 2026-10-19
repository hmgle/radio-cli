use super::{
    command::HELP,
    state::{App, Focus, Mode, Source},
    worker::View,
};
use crate::model::Station;
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, List, ListItem, Paragraph, Wrap},
};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

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
        "码率未知".into()
    } else {
        format!("{} kbps", station.bitrate)
    }
}
fn codec(station: &Station) -> String {
    if station.codec.is_empty() || station.codec.eq_ignore_ascii_case("unknown") {
        "编码未知".into()
    } else {
        crate::clean_text(&station.codec)
    }
}
fn value(text: &str) -> String {
    if text.trim().is_empty() {
        "未知".into()
    } else {
        crate::clean_text(text)
    }
}

pub(super) fn fit(text: &str, width: usize) -> String {
    let text = crate::clean_text(text);
    if text.width() <= width {
        return format!("{text}{}", " ".repeat(width - text.width()));
    }
    if width == 0 {
        return String::new();
    }
    let mut result = String::new();
    let mut used = 0;
    for ch in text.chars() {
        let cells = ch.width().unwrap_or(0);
        if used + cells >= width {
            break;
        }
        result.push(ch);
        used += cells;
    }
    result.push('…');
    result.push_str(&" ".repeat(width - used - 1));
    result
}

pub(super) fn draw(frame: &mut Frame, app: &mut App, view: &View) {
    draw_content(frame, app, view);
    let theme = app.theme;
    if theme == crate::config::Theme::Default {
        return;
    }
    for cell in &mut frame.buffer_mut().content {
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

fn draw_content(frame: &mut Frame, app: &mut App, view: &View) {
    let area = frame.area();
    app.rects = [Rect::default(); 5];
    app.source_rows = [Rect::default(); 4];
    app.filter_summary = Rect::default();
    app.update_size(area.width, area.height);
    if app.too_small {
        frame.render_widget(
            Paragraph::new("radio · 请将终端调整到至少 40×16；q 退出"),
            area,
        );
        return;
    }
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(if area.width >= 100 && area.height >= 20 {
                5
            } else {
                4
            }),
            Constraint::Min(3),
            Constraint::Length(if view.task_active || app.focus == Focus::Activity {
                4
            } else {
                1
            }),
            Constraint::Length(3),
        ])
        .split(area);
    app.rects[Focus::Player.index()] = vertical[0];
    app.rects[Focus::Activity.index()] = vertical[2];
    if area.width >= 100 && area.height >= 20 {
        let columns = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Length(18),
                Constraint::Percentage(48),
                Constraint::Min(25),
            ])
            .split(vertical[1]);
        app.rects[Focus::Sources.index()] = columns[0];
        app.rects[Focus::List.index()] = columns[1];
        app.rects[Focus::Details.index()] = columns[2];
    } else {
        let sources = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(3), Constraint::Min(0)])
            .split(vertical[1]);
        app.rects[Focus::Sources.index()] = sources[0];
        // Narrow screens devote the content region to the list; 3 opens details.
        app.rects[Focus::List.index()] = sources[1];
        app.rects[Focus::Details.index()] = sources[1];
    }
    let normal = app.mode == Mode::Normal && !app.help && app.filter_edit.is_none();
    let active = |focus| normal && app.focus == focus;
    let playing = match &view.status.playback_state {
        crate::engine::PlaybackState::Failed { reason } => {
            format!("播放失败: {} · ?", fit(reason, 12).trim_end())
        }
        state => state.label().into(),
    };
    let recording = format!(
        "{}{}{}",
        view.status.recording_state.label(),
        if matches!(
            view.status.recording_state,
            crate::engine::RecordingState::Recording
        ) {
            format!(
                " ● REC {}s{}",
                view.status.recording_ms / 1000,
                if view.status.paused() {
                    " · 播放暂停，录音继续"
                } else {
                    ""
                }
            )
        } else {
            String::new()
        },
        if matches!(
            view.status.recording_state,
            crate::engine::RecordingState::Failed { .. }
        ) {
            " · ? 详情"
        } else {
            ""
        }
    );
    let volume = format!(
        "{}%{}",
        view.status.volume,
        if view.status.muted { " · 静音" } else { "" }
    );
    let player_text = if let Some(station) = &view.status.station {
        format!(
            "{} · {} · 音量 {}\n{} · {} · {}（目录）\n{} · {} · {}",
            playing,
            fit(
                &value(&station.name),
                usize::from(area.width.saturating_sub(40))
            )
            .trim_end(),
            volume,
            recording,
            codec(station),
            bitrate(station),
            value(&station.countrycode),
            value(&station.language),
            if view.status.title.is_empty() {
                "暂无曲名".into()
            } else {
                value(&view.status.title)
            }
        )
    } else {
        format!(
            "尚未播放 · 音量 {}{}\n在列表选择电台，Enter 播放；:sync 同步目录",
            volume, recording
        )
    };
    let player_text = if area.width < 100 || area.height < 20 {
        let lines = player_text.lines().collect::<Vec<_>>();
        format!("{}\n{}", lines[0], lines[1..].join(" · "))
    } else {
        player_text
    };
    frame.render_widget(
        Paragraph::new(player_text).block(panel(
            "radio · 4 播放器 · 当前电台".into(),
            active(Focus::Player),
        )),
        app.rects[Focus::Player.index()],
    );

    let source_rect = app.rects[Focus::Sources.index()];
    frame.render_widget(panel("1 来源".into(), active(Focus::Sources)), source_rect);
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
                Paragraph::new("j/k 选择\nEnter 进入\n[ / ] 切分类")
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
                Paragraph::new(source.label()).style(if app.source == source {
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
    let show_details = app.rects[Focus::List.index()] != app.rects[Focus::Details.index()]
        || app.focus == Focus::Details;
    if app.rects[Focus::List.index()] != app.rects[Focus::Details.index()]
        || app.focus != Focus::Details
    {
        let title = format!(
            "{} {} · {}/{} 项 · 页 {}",
            if area.width < 100 {
                "[2/3] 列表"
            } else {
                "2"
            },
            app.source.label(),
            app.length(view),
            if app.source == Source::Recordings {
                view.recordings_total
            } else {
                view.total
            },
            app.filter.offset / app.filter.limit.max(1) + 1
        );
        let items: Vec<ListItem> = if app.filter != view.filter {
            vec![]
        } else if app.source == Source::Recordings {
            view.recordings
                .iter()
                .map(|r| {
                    ListItem::new(format!(
                        "{} · {} · {}",
                        value(&r.station_name),
                        r.status,
                        crate::clean_text(&r.path)
                    ))
                })
                .collect()
        } else {
            view.rows
                .iter()
                .map(|s| {
                    let current = view.status.playing()
                        && view
                            .status
                            .station
                            .as_ref()
                            .is_some_and(|now| now.stationuuid == s.stationuuid);
                    ListItem::new(format!(
                        "{}{} {} [{}] {} {}{}",
                        if current { "▶" } else { " " },
                        if s.favorite { "★" } else { " " },
                        fit(
                            &value(&s.name),
                            usize::from(app.rects[Focus::List.index()].width.saturating_sub(37))
                        ),
                        fit(&s.countrycode, 2),
                        fit(&bitrate(s), 10),
                        fit(&codec(s), 7),
                        if s.active { "" } else { " [已失效]" }
                    ))
                })
                .collect()
        };
        let rect = app.rects[Focus::List.index()];
        if items.is_empty() {
            frame.render_widget(
                Paragraph::new(
                    if app.filter != view.filter && app.source != Source::Recordings {
                        "正在更新列表…"
                    } else if view.task_active {
                        "目录任务进行中…\n可以继续使用 : 命令"
                    } else {
                        "暂无匹配项\n:sync 同步 · / 搜索 · F 筛选 · ] 切换来源"
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
            view.recordings.get(app.list.selected().unwrap_or(0)).map(|r| {
                format!(
                    "{}\n状态: {}\n编码: {} / {}\n时长: {}\n大小: {} bytes\n开始: {}\n结束: {}\n电台 UUID: {}\n地址: {}\n文件: {}\n{}{}",
                    value(&r.station_name),
                    r.status,
                    r.codec,
                    r.container,
                    r.media_duration_ms
                        .map(|ms| format!("{:.1}s", ms as f64 / 1000.))
                        .unwrap_or_else(|| "未知".into()),
                    r.size_bytes,
                    r.started_at,
                    r.stopped_at.as_deref().unwrap_or("录制中"),
                    r.stationuuid,
                    crate::clean_text(&r.source_url_resolved),
                    crate::clean_text(&r.path),
                    if r.reconnects > 0 {
                        format!("{}\n", crate::t!("recording_reconnected", "count" => r.reconnects))
                    } else {
                        String::new()
                    },
                    r.error.as_deref().map(|error| if app.recording_error_expanded {
                        error.lines().map(crate::clean_text).collect::<Vec<_>>().join("\n")
                    } else {
                        format!("{} · e 展开错误", crate::clean_text(error.lines().next().unwrap_or("")))
                    }).unwrap_or_default()
                )
            }).unwrap_or_else(|| "暂无录音记录".into())
        } else if app.inspect_info && view.inspected_request != app.info_request {
            "正在查询详情…".into()
        } else if app.inspect_info && view.inspected_error.is_some() {
            view.inspected_error.clone().unwrap()
        } else if let Some(station) = if app.inspect_info {
            view.inspected.as_ref()
        } else {
            app.selected(view)
        } {
            let current = view
                .status
                .station
                .as_ref()
                .filter(|s| s.stationuuid == station.stationuuid);
            format!(
                "{}{}\n国家: {} ({})\n语言: {}\n地区: {}\n码率: {}（目录）\n编码: {}（目录）\nHLS: {}\n标签: {}\n收藏: {} · 目录: {}\nUUID: {}\n流地址: {}\n备用地址: {}\n{}官网: {}",
                value(&station.name),
                if current.is_some() && view.status.playing() {
                    " · 正在播放"
                } else {
                    " · 列表选中 / 指定电台"
                },
                value(&station.country),
                value(&station.countrycode),
                value(&station.language),
                value(&station.state),
                bitrate(station),
                codec(station),
                if station.hls != 0 { "是" } else { "否" },
                value(&station.tags),
                if station.favorite { "是" } else { "否" },
                if station.custom {
                    "自定义"
                } else if station.active {
                    "有效"
                } else {
                    "已失效"
                },
                station.stationuuid,
                crate::clean_text(station.primary_url()),
                if let Some(error) = view.backups_errors.get(&station.stationuuid) {
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
                if current.is_some() {
                    format!(
                        "当前播放请求地址: {}\n",
                        crate::clean_text(&view.status.source_url)
                    )
                } else {
                    String::new()
                },
                value(&station.homepage)
            )
        } else {
            "选择电台查看码率、编码、国家、标签和地址。\n4 查看当前播放电台。".into()
        };
        frame.render_widget(
            scroll_text(
                &text,
                app.rects[Focus::Details.index()],
                &mut app.details_scroll,
            )
            .block(panel(
                if area.width < 100 {
                    "[2/3] 详情 · 2 返回 · j/k 滚动"
                } else {
                    "3 详情 · j/k 滚动"
                }
                .into(),
                active(Focus::Details),
            )),
            app.rects[Focus::Details.index()],
        );
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
            .map(|line| Line::from(line.clone())),
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
                "5 任务与消息 · :sync / :sync-cancel".into(),
                active(Focus::Activity),
            )),
            rect,
        );
    }

    let bottom = vertical[3];
    if app.mode != Mode::Normal {
        let label = if app.mode == Mode::Command {
            "COMMAND :"
        } else {
            "SEARCH /"
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
                "Enter 执行 · Esc 返回 · Tab 补全 · ↑/↓ 历史 · :help 帮助"
            } else {
                "Enter 确认 · Esc 恢复搜索 · ←/→ 移动光标"
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
        if app.source != Source::Recordings {
            app.filter_summary = Rect::new(bottom.x, bottom.y, bottom.width, 1);
        }
        let filter = if app.source == Source::Recordings {
            "本地录音 · 不使用电台筛选".into()
        } else {
            crate::clean_text(&format!(
                "/{}  {} {} {} ≥{}k",
                crate::clean_text(&app.filter.query),
                app.filter.country.as_deref().unwrap_or("全部国家"),
                app.filter.language.as_deref().unwrap_or("全部语言"),
                app.filter.tag.as_deref().unwrap_or("全部标签"),
                app.filter.min_bitrate
            ))
        };
        let context = context_hint(app.source, app.focus);
        frame.render_widget(
            Paragraph::new(vec![
                Line::from(vec![
                    Span::styled(
                        format!(" NORMAL · {} ", app.focus.label()),
                        Style::default().bg(Color::Cyan).fg(Color::Black),
                    ),
                    Span::raw(filter),
                ]),
                Line::from(if app.message.is_empty() {
                    context
                } else {
                    &app.message
                }),
                Line::from(
                    "Tab/Shift-Tab 焦点 · 1–5 面板 · [/] 分类 · : 命令 · / 搜索 · ? 帮助 · q 退出",
                ),
            ]),
            bottom,
        );
    }
    if let Some((selected, values)) = &app.filter_edit {
        let rect = popup(area, 72, 18);
        frame.render_widget(Clear, rect);
        let names = ["国家代码", "语言", "标签", "最低码率"];
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
                                rect.width.saturating_sub(16),
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
            .chain([
                Line::from("Tab 下一项 · ↑/↓ 候选 · Enter 应用 · Ctrl-U 清空 · Esc 取消"),
                Line::from(crate::clean_text(&app.message)),
            ])
            .collect::<Vec<_>>();
        if *selected < 3 {
            let candidates = app.facet_candidates(view, *selected);
            let count = usize::from(rect.height.saturating_sub(9)).min(8);
            let start = app
                .facet_index
                .unwrap_or(0)
                .saturating_sub(count / 2)
                .min(candidates.len().saturating_sub(count));
            text.push(Line::from(if view.facets_loading {
                "正在加载本地候选…"
            } else {
                "本地候选（电台数，最多100项；输入缩小）:"
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
                        crate::clean_text(&facet.value),
                        facet.count
                    ))
                },
            ));
        }
        frame.render_widget(Paragraph::new(text).block(panel("筛选".into(), true)), rect);
        let label_width = format!("› {}: ", names[*selected]).width() as u16;
        let (_, cursor) = input_window(
            &values[*selected],
            app.filter_input.cursor.min(values[*selected].len()),
            rect.width.saturating_sub(16),
        );
        frame.set_cursor_position((
            rect.x + 1 + label_width + cursor,
            rect.y + 1 + *selected as u16,
        ));
    }
    if app.help {
        let rect = popup(area, 88, area.height.saturating_sub(2));
        frame.render_widget(Clear, rect);
        frame.render_widget(
            scroll_text(
                &app.help_content.clone().unwrap_or_else(|| {
                    if app.jump_binding.is_empty() {
                        return HELP.into();
                    }
                    format!(
                        "{HELP}\n{}  定位当前播放电台（可在 keybindings.jump_to_playing 配置）",
                        app.jump_binding
                    )
                }),
                rect,
                &mut app.help_scroll,
            )
            .block(panel("帮助 · j/k 滚动 · Esc 关闭".into(), true)),
            rect,
        );
    }
}

/// Hard-wrap by terminal cells so long URLs remain readable and scrolling cannot
/// move beyond the last page. Keep slices on UTF-8 boundaries, including CJK.
fn scroll_text<'a>(text: &'a str, rect: Rect, scroll: &mut u16) -> Paragraph<'a> {
    let width = usize::from(rect.width.saturating_sub(2).max(1));
    let mut lines = Vec::new();
    for line in text.lines() {
        let line = crate::clean_text(line);
        let mut start = 0;
        let mut cells = 0;
        for (index, c) in line.char_indices() {
            let size = c.width().unwrap_or(0);
            if cells + size > width && index > start {
                lines.push(Line::from(line[start..index].to_owned()));
                start = index;
                cells = 0;
            }
            cells += size;
        }
        lines.push(Line::from(line[start..].to_owned()));
    }
    let visible = usize::from(rect.height.saturating_sub(2));
    let maximum = lines.len().saturating_sub(visible).min(u16::MAX as usize) as u16;
    *scroll = (*scroll).min(maximum);
    Paragraph::new(lines).scroll((*scroll, 0))
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

pub(super) fn context_hint(source: Source, focus: Focus) -> &'static str {
    match (source, focus) {
        (_, Focus::Sources) => "j/k 分类 · Enter 进入列表",
        (Source::Recordings, Focus::List) => {
            "Enter 播放 · D 删除 · o 目录 · e 错误 · PgUp/PgDn 翻页"
        }
        (_, Focus::List) => "j/k 选择 · Enter 播放 · f 收藏 · gg/G 首尾 · PgUp/PgDn 翻页",
        (_, Focus::Details) => "j/k / PgUp/PgDn 滚动详情",
        (_, Focus::Player) => "Enter 当前电台详情 · Space 暂停 · r 录音 · +/- 音量 · :stop 停止",
        (_, Focus::Activity) => "j/k 滚动消息 · :sync 同步 · :sync-cancel 取消",
    }
}

pub(super) fn input_geometry(label: &str, width: u16) -> (u16, u16) {
    let start = label.width().saturating_add(1).min(usize::from(width)) as u16;
    (
        width.saturating_sub(start),
        start.min(width.saturating_sub(1)),
    )
}

/// Horizontally scroll input by display cells while keeping the Unicode cursor visible.
fn input_window(text: &str, cursor: usize, width: u16) -> (String, u16) {
    if width == 0 {
        return (String::new(), 0);
    }
    let mut start = 0;
    let mut cells = text[..cursor]
        .chars()
        .map(|c| c.width().unwrap_or(0))
        .sum::<usize>();
    while cells >= usize::from(width) && start < cursor {
        let c = text[start..].chars().next().unwrap();
        start += c.len_utf8();
        cells = cells.saturating_sub(c.width().unwrap_or(0));
    }
    let mut end = start;
    let mut used = 0;
    for c in text[start..].chars() {
        let n = c.width().unwrap_or(0);
        if used + n > usize::from(width) {
            break;
        }
        used += n;
        end += c.len_utf8();
    }
    (text[start..end].into(), cells as u16)
}
