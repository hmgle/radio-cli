use super::{
    command::{self, Command, Parsed},
    render,
    state::{App, Editor, Focus, Mode, Source},
    worker::{self, View},
};
use crate::{
    engine::{Engine, Status},
    model::{Filter, Station},
    store::Store,
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{Terminal, backend::TestBackend, style::Color};
use std::{
    io::{Read, Write},
    net::TcpListener,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

#[test]
fn subtitle_history_does_not_replace_help_after_resizing_and_escape() {
    let data = view();
    let mut app = App::default();
    let (tx, _rx) = command::channel(16);
    app.help = true;
    app.subtitle_history = true;
    app.update_size(39, 15);
    app.key(key(KeyCode::Esc), &data, &tx);
    app.update_size(80, 24);
    app.key(key(KeyCode::Char('?')), &data, &tx);
    assert!(app.help);
    assert!(!app.subtitle_history);
    assert!(app.help_content.is_none());
}

#[test]
fn subtitle_updates_redraw_and_report_persistent_failures_once() {
    let mut data = view();
    let mut snapshot = crate::speech::Snapshot {
        enabled: true,
        revision: 1,
        ..Default::default()
    };
    let revision = data.immediate_revision;
    data.update_subtitles(&snapshot);
    assert!(data.immediate_revision > revision);
    snapshot.failed = true;
    snapshot.revision += 1;
    snapshot.message = "model failure with complete diagnostic details".into();
    data.update_subtitles(&snapshot);
    assert_eq!(data.operation_error, snapshot.message);
    let error_id = data.operation_error_id;
    data.update_subtitles(&snapshot);
    assert_eq!(data.operation_error_id, error_id);
    let mut app = App::default();
    let (tx, _rx) = command::channel(16);
    app.key(key(KeyCode::Char('?')), &data, &tx);
    assert!(app.help_content.unwrap().contains(&snapshot.message));
}

#[test]
fn subtitle_strip_preserves_layout_and_command_bar_at_height_boundaries() {
    for height in [16, 20, 23, 24] {
        let mut data = view();
        data.task_active = true;
        let mut app = App::default();
        let mut terminal = Terminal::new(TestBackend::new(100, height)).unwrap();
        terminal.draw(|f| render::draw(f, &mut app, &data)).unwrap();
        let before = app.rects;
        data.subtitles.enabled = true;
        data.subtitles.message = "Loading speech model".into();
        app.mode = Mode::Command;
        app.editor.begin("subtitles history");
        terminal.draw(|f| render::draw(f, &mut app, &data)).unwrap();
        assert_eq!(
            app.rects[Focus::Sources.index()].width,
            before[Focus::Sources.index()].width
        );
        assert_eq!(
            app.rects[Focus::Activity.index()].height,
            before[Focus::Activity.index()].height
        );
        let rendered = text(&terminal);
        assert!(rendered.contains("Loading speech model"));
        assert!(!rendered.contains("Loading speech model ·"));
        assert!(rendered.contains("subtitles history"));
        let rows: Vec<_> = rendered.lines().collect();
        let caption = rows
            .iter()
            .position(|row| row.contains("Loading speech model"))
            .unwrap();
        let command = rows
            .iter()
            .position(|row| row.contains("subtitles history"))
            .unwrap();
        assert!(caption < command);
        assert!(command >= usize::from(height - 3));
    }
}

#[test]
fn single_row_subtitles_keep_unspaced_chinese_visible() {
    for width in [40, 50] {
        let mut data = view();
        data.subtitles.enabled = true;
        data.subtitles.message = "Listening · delayed subtitles".into();
        Arc::make_mut(&mut data.subtitles.history).push_back(crate::speech::Segment {
            epoch: 1,
            start_ms: 0,
            end_ms: 1000,
            text: "今日廣東天氣晴朗適合出門聽廣播".into(),
        });
        let mut app = App::default();
        let mut terminal = Terminal::new(TestBackend::new(width, 20)).unwrap();
        terminal.draw(|f| render::draw(f, &mut app, &data)).unwrap();
        assert!(text(&terminal).contains("今日廣"));
    }
}

#[test]
fn three_row_subtitles_keep_history_visible_beside_long_failures() {
    let mut data = view();
    data.subtitles.enabled = true;
    data.subtitles.message = "damaged model: ".to_owned() + &"very long diagnostic ".repeat(8);
    for i in 0..4 {
        Arc::make_mut(&mut data.subtitles.history).push_back(crate::speech::Segment {
            epoch: 1,
            start_ms: i * 1000,
            end_ms: i * 1000 + 500,
            text: format!("recent caption {i}"),
        });
    }
    let mut app = App::default();
    let mut terminal = Terminal::new(TestBackend::new(40, 24)).unwrap();
    terminal.draw(|f| render::draw(f, &mut app, &data)).unwrap();
    let rendered = text(&terminal);
    assert!(rendered.contains("damaged model:"));
    assert!(rendered.contains("recent caption 2"));
    assert!(rendered.contains("recent caption 3"));
}

#[test]
fn subtitle_strip_wraps_long_status_messages() {
    let mut data = view();
    data.subtitles.enabled = true;
    data.subtitles.message = format!("{} repair command", "x".repeat(90));
    let mut app = App::default();
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    terminal.draw(|f| render::draw(f, &mut app, &data)).unwrap();
    assert!(text(&terminal).contains("repair command"));
}

#[test]
fn subtitle_history_marks_reconnect_epochs() {
    let mut data = view();
    for epoch in [1, 2] {
        Arc::make_mut(&mut data.subtitles.history).push_back(crate::speech::Segment {
            epoch,
            start_ms: 0,
            end_ms: 100,
            text: format!("epoch {epoch}"),
        });
    }
    let mut app = App::default();
    app.help = true;
    app.subtitle_history = true;
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    terminal.draw(|f| render::draw(f, &mut app, &data)).unwrap();
    let rendered = text(&terminal);
    assert!(rendered.contains("epoch 1") && rendered.contains("epoch 2"));
    assert!(rendered.contains(crate::t!("subtitle_interrupted")));
}

#[test]
fn subtitle_commands_toggle_and_select_models_without_running_management() {
    let data = view();
    for action in ["on", "off", "toggle", "clear", "retry"] {
        let parsed = command::parse(
            &format!("subtitles {action}"),
            None,
            data.status.station.as_ref(),
            &data.filter,
        )
        .unwrap();
        assert!(matches!(parsed, Parsed::Worker(Command::Subtitles(value)) if value == action));
    }
    assert!(matches!(
        command::parse("subtitles history", None, None, &data.filter).unwrap(),
        Parsed::SubtitleHistory
    ));
    assert!(command::parse("subtitles setup", None, None, &data.filter).is_err());
    assert!(matches!(
        command::parse(
            "set subtitles.model qwen3-asr-0.6b-int8",
            None,
            None,
            &data.filter
        )
        .unwrap(),
        Parsed::Worker(Command::SubtitleModel(_))
    ));
}

#[test]
fn subtitle_history_follows_latest_until_user_scrolls_and_keeps_layout_usable() {
    let mut data = view();
    data.subtitles.enabled = true;
    data.subtitles.message = "Listening".into();
    for i in 0..80 {
        Arc::make_mut(&mut data.subtitles.history).push_back(crate::speech::Segment {
            epoch: 1,
            start_ms: i * 1000,
            end_ms: i * 1000 + 800,
            text: format!("spoken sentence {i}"),
        });
    }
    for (width, height) in [(40, 16), (80, 24), (160, 48)] {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        let mut app = App::default();
        app.subtitle_history = true;
        app.subtitle_follow = true;
        app.help = true;
        terminal.draw(|f| render::draw(f, &mut app, &data)).unwrap();
        assert!(text(&terminal).contains("spoken sentence 79"));
        assert!(app.rects[Focus::List.index()].height > 0);
        let (tx, _rx) = command::channel(16);
        app.key(key(KeyCode::Home), &data, &tx);
        terminal.draw(|f| render::draw(f, &mut app, &data)).unwrap();
        assert_eq!(app.help_scroll, 0);
        assert!(text(&terminal).contains("spoken sentence 0"));
        Arc::make_mut(&mut data.subtitles.history).push_back(crate::speech::Segment {
            epoch: 1,
            start_ms: 81000,
            end_ms: 81800,
            text: "new subtitle".into(),
        });
        terminal.draw(|f| render::draw(f, &mut app, &data)).unwrap();
        assert_eq!(app.help_scroll, 0);
        app.key(key(KeyCode::End), &data, &tx);
        terminal.draw(|f| render::draw(f, &mut app, &data)).unwrap();
        assert!(text(&terminal).contains("new subtitle"));
        app.key(key(KeyCode::Esc), &data, &tx);
        assert!(!app.help && !app.subtitle_history);
    }
}
fn station(name: &str) -> Station {
    Station {
        stationuuid: uuid::Uuid::new_v4().to_string(),
        name: name.into(),
        active: true,
        bitrate: 192,
        codec: "AAC".into(),
        country: "France".into(),
        countrycode: "FR".into(),
        language: "french".into(),
        ..Station::default()
    }
}
fn view() -> View {
    let playing = station("Current Station");
    let mut selected = station("Selected Station");
    selected.bitrate = 96;
    selected.codec = "MP3".into();
    let mut data = View::new(Status {
        station: Some(playing),
        title: "Song title".into(),
        message: "Playing".into(),
        playback_state: crate::engine::PlaybackState::Playing,
        volume: 80,
        muted: false,
        recording_state: crate::engine::RecordingState::Idle,
        recording_ms: 0,
        recording_path: String::new(),
        source_url: "https://example.com/backup".into(),
    });
    data.rows = vec![selected, station("Another Station")].into();
    data.page = Some(worker::PageState {
        generation: 0,
        requested: data.filter.clone(),
        phase: worker::PagePhase::Ready,
    });
    data
}
fn text(terminal: &Terminal<TestBackend>) -> String {
    let buffer = terminal.backend().buffer();
    let mut text = String::new();
    for y in buffer.area.y..buffer.area.bottom() {
        let mut x = buffer.area.x;
        while x < buffer.area.right() {
            let symbol = buffer[(x, y)].symbol();
            text.push_str(symbol);
            x += unicode_width::UnicodeWidthStr::width(symbol).max(1) as u16;
        }
        text.push('\n');
    }
    text
}

fn assert_no_cjk(rendered: &str) {
    assert!(
        !rendered
            .chars()
            .any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c)),
        "{rendered}"
    );
}

#[test]
fn station_details_localize_territory_codes_without_changing_catalog_values() {
    let data = view();
    for (code, original, expected, keeps_upstream) in [
        ("TW", "Taiwan, Republic Of China", "台湾 (TW)", false),
        ("HK", "Hong Kong", "中国香港特别行政区 (HK)", false),
        ("MO", "Macao", "中国澳门特别行政区 (MO)", false),
        ("FR", "France", "法国 (FR)", false),
        (" us ", "United States", "美国 (US)", false),
        // Unknown and missing codes fall back to the upstream catalog name
        // instead of discarding it.
        (
            "QZ",
            "Unrecognized upstream name",
            "Unrecognized upstream name (QZ)",
            true,
        ),
        ("", "Missing upstream code", "Missing upstream code", true),
    ] {
        let mut row = station("Catalog station");
        row.country = original.into();
        row.countrycode = code.into();
        row.state = "Catalog subdivision".into();
        let details = render::station_details(&App::default(), &row, &data);
        let chinese = crate::territories::display_in(crate::messages::Locale::ZhCn, code, original);
        assert_eq!(chinese, expected);
        let country = crate::territories::display_in(crate::messages::Locale::En, code, original);
        assert!(
            details.contains(&format!("\nCountry:\t{country}\n")),
            "{code}: {details}"
        );
        assert!(details.contains("\nState/province/area:\tCatalog subdivision\n"));
        assert_eq!(
            keeps_upstream,
            chinese.contains(original),
            "{code}: {details}"
        );
        let json = serde_json::to_value(&row).unwrap();
        assert_eq!(json["country"], original);
        assert_eq!(json["countrycode"], code);
    }
}

#[test]
fn help_popup_border_survives_mixed_width_station_names() {
    let mut data = view();
    data.rows = vec![
        station("2AC澳洲华人电台粤语广播"),
        station("CNR-15 中国交通广播"),
        station("CNR-4 经典音乐广播"),
        station("故事广播-阿基米德故事会"),
    ]
    .into();
    // Widths chosen so the popup's left border lands on the list panel at
    // alignments where a wide character's trailing cell sits on the border.
    for width in [130u16, 160] {
        let height = 30u16;
        let mut app = App::default();
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| render::draw(frame, &mut app, &data))
            .unwrap();
        app.help = true;
        terminal
            .draw(|frame| render::draw(frame, &mut app, &data))
            .unwrap();
        let buf = terminal.backend().buffer();
        // Mirror popup(): the help popup is 88 wide (or the whole area when
        // narrower) and centred, so its left border sits at this column.
        let popup_w = 88u16.min(width.saturating_sub(2));
        let left = (width - popup_w) / 2;
        let popup_h = height.saturating_sub(2);
        let top = (height - popup_h) / 2;
        let mut broken = Vec::new();
        for y in top..top + popup_h {
            let expected = match y {
                t if t == top => "┏",
                b if b == top + popup_h - 1 => "┗",
                _ => "┃",
            };
            let sym = buf[(left, y)].symbol();
            if sym != expected {
                broken.push(format!("w={width} y={y} sym={sym:?} expected={expected:?}"));
            }
        }
        assert!(broken.is_empty(), "left border broken at: {broken:?}");
    }
}

#[test]
fn help_popup_border_survives_wide_characters() {
    let data = view();
    let mut app = App::default();
    let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
    // Draw once without the popup so wide CJK characters land left of the popup.
    terminal
        .draw(|frame| render::draw(frame, &mut app, &data))
        .unwrap();
    app.help = true;
    terminal
        .draw(|frame| render::draw(frame, &mut app, &data))
        .unwrap();
    let buf = terminal.backend().buffer();
    let mut broken = Vec::new();
    for y in 1..23 {
        let expected = match y {
            1 => "┏",
            22 => "┗",
            _ => "┃",
        };
        let sym = buf[(6, y)].symbol();
        if sym != expected {
            broken.push(format!("y={y} sym={sym:?} expected={expected:?}"));
        }
    }
    assert!(broken.is_empty(), "left border broken at: {broken:?}");
}

#[test]
fn filter_popup_border_survives_mixed_width_station_names() {
    let mut data = view();
    data.rows = vec![
        station("2AC澳洲华人电台粤语广播"),
        station("CNR-15 中国交通广播"),
        station("CNR-4 经典音乐广播"),
        station("故事广播-阿基米德故事会"),
    ]
    .into();
    // Widths where the filter popup's left border lands on the list panel at
    // alignments that put a wide character's trailing cell on the border.
    for width in [76u16, 130] {
        let height = 24u16;
        let mut app = App::default();
        app.filter_edit = Some((0, Default::default()));
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| render::draw(frame, &mut app, &data))
            .unwrap();
        let buf = terminal.backend().buffer();
        // Mirror popup(area, 72, 18): the filter popup is 72 wide (or the whole
        // area when narrower) and centred.
        let popup_w = 72u16.min(width.saturating_sub(2));
        let left = (width - popup_w) / 2;
        let popup_h = 18u16.min(height.saturating_sub(2));
        let top = (height - popup_h) / 2;
        let mut broken = Vec::new();
        for y in top..top + popup_h {
            let expected = match y {
                t if t == top => "┏",
                b if b == top + popup_h - 1 => "┗",
                _ => "┃",
            };
            let sym = buf[(left, y)].symbol();
            if sym != expected {
                broken.push(format!("w={width} y={y} sym={sym:?} expected={expected:?}"));
            }
        }
        assert!(
            broken.is_empty(),
            "filter left border broken at: {broken:?}"
        );
    }
}

#[test]
fn help_popup_preserves_background_right_of_the_border() {
    let mut data = view();
    // A long track name so the player panel's title line reaches the right
    // margin, immediately right of the help popup's right border.
    data.status.title = "VeryLongTrackNameThatExtendsAcrossThePlayerPanelToCol94".into();
    let width = 100u16;
    let height = 24u16;
    let mut app = App::default();
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal
        .draw(|frame| render::draw(frame, &mut app, &data))
        .unwrap();
    let base = terminal.backend().buffer().clone();
    app.help = true;
    terminal
        .draw(|frame| render::draw(frame, &mut app, &data))
        .unwrap();
    let buf = terminal.backend().buffer();
    // At 100x24 the help popup spans x=6..=93; column 94 is the first background
    // column to its right and must survive the popup's Clear.
    let right = 94u16;
    let present: Vec<u16> = (0..height)
        .filter(|&y| base[(right, y)].symbol() != " ")
        .collect();
    assert!(
        !present.is_empty(),
        "fixture must place content at column {right} for this test to be meaningful"
    );
    let mut erased = Vec::new();
    for y in present {
        if buf[(right, y)].symbol() != base[(right, y)].symbol() {
            erased.push(format!(
                "y={y} base={:?} now={:?}",
                base[(right, y)].symbol(),
                buf[(right, y)].symbol()
            ));
        }
    }
    assert!(erased.is_empty(), "column {right} erased at: {erased:?}");
}

fn help_text(locale: crate::messages::Locale, binding: &str, width: u16) -> String {
    super::help::lines(locale, binding, width)
        .iter()
        .map(|line| line.to_string())
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn help_notice_follows_the_selected_locale() {
    let english = help_text(crate::messages::Locale::En, "", 86);
    let chinese = help_text(crate::messages::Locale::ZhCn, "", 86);
    assert!(english.contains("Radio Browser"), "{english}");
    assert!(!english.contains("默认电台目录"), "{english}");
    assert!(
        chinese.contains("默认电台目录来自 Radio Browser"),
        "{chinese}"
    );
    for (locale, text) in [
        (crate::messages::Locale::En, &english),
        (crate::messages::Locale::ZhCn, &chinese),
    ] {
        assert!(
            text.starts_with(crate::messages::text_in(locale, "help_nav_title")),
            "{locale:?}"
        );
        assert!(!text.contains("Unknown message"), "{locale:?}");
    }
    let with_binding = help_text(crate::messages::Locale::En, "''", 86);
    assert!(
        with_binding.contains("Locate the playing station"),
        "{with_binding}"
    );
    assert!(
        with_binding
            .lines()
            .any(|line| line.trim_start().starts_with("''")),
        "{with_binding}"
    );
    assert!(!english.contains("Locate the playing station"));
    assert_no_cjk(&with_binding);
}

#[test]
fn help_sections_keep_the_same_shape_in_both_locales() {
    use super::help::Entry;
    let has_cjk = |text: &str| text.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c));
    let english = super::help::sections(crate::messages::Locale::En, "<F6>");
    let chinese = super::help::sections(crate::messages::Locale::ZhCn, "<F6>");
    assert_eq!(english.len(), chinese.len());
    for (en, zh) in english.iter().zip(&chinese) {
        assert!(!en.title.is_empty() && !zh.title.is_empty());
        assert_eq!(en.entries.len(), zh.entries.len(), "{}", en.title);
        for (a, b) in en.entries.iter().zip(&zh.entries) {
            match (a, b) {
                (Entry::Note(_), Entry::Note(_)) => {}
                (
                    Entry::Item {
                        keys: en_keys,
                        detail: en_detail,
                    },
                    Entry::Item {
                        keys: zh_keys,
                        detail: zh_detail,
                    },
                ) => {
                    assert!(!en_keys.is_empty() && !zh_keys.is_empty(), "{}", en.title);
                    assert_eq!(en_detail.is_empty(), zh_detail.is_empty(), "{en_keys}");
                    // Key names that are not translated must be identical.
                    if !has_cjk(zh_keys) {
                        assert_eq!(en_keys, zh_keys, "{}", en.title);
                    }
                }
                _ => panic!("{}: {a:?} vs {b:?}", en.title),
            }
        }
    }
}

#[test]
fn help_aligns_descriptions_by_display_width() {
    let detail_column = |lines: &[String], key: &str, detail: &str| {
        let line = lines
            .iter()
            .find(|line| line.trim_start().starts_with(key) && line.contains(detail))
            .unwrap_or_else(|| panic!("no line with {key:?} and {detail:?}"));
        super::text::width(&line[..line.find(detail).unwrap()])
    };
    let lines = |locale| -> Vec<String> {
        super::help::lines(locale, "", 86)
            .iter()
            .map(|line| line.to_string())
            .collect()
    };
    let english = lines(crate::messages::Locale::En);
    let column = detail_column(&english, "Tab / Shift-Tab", "Switch panels");
    assert_eq!(detail_column(&english, "gg / G", "First/last item"), column);
    assert_eq!(detail_column(&english, "1–5", "Focus a panel"), column);
    assert_eq!(
        detail_column(
            &english,
            "Ctrl-h/j/k/l or Ctrl-arrows",
            "Switch panels by direction"
        ),
        column
    );

    // Wide characters in a description or key never shift the column.
    let chinese = lines(crate::messages::Locale::ZhCn);
    let column = detail_column(&chinese, "Tab / Shift-Tab", "切换面板");
    assert_eq!(detail_column(&chinese, "gg / G", "首项/末项"), column);
    assert_eq!(
        detail_column(&chinese, "Ctrl-u / Ctrl-d", "移动十项"),
        column
    );
    assert_eq!(
        detail_column(&chinese, "Ctrl-h/j/k/l 或 Ctrl-方向键", "按方向切换面板"),
        column
    );
    let commands = detail_column(&chinese, ":refresh", "刷新当前列表");
    assert_eq!(detail_column(&chinese, ":history", "最近播放"), commands);
    assert_eq!(
        detail_column(&chinese, ":random [搜索筛选参数]", "不传参数时沿用当前筛选"),
        commands
    );
}

#[test]
fn help_documents_the_keys_the_screens_rely_on() {
    use crate::messages::Locale;
    let expected: &[(Locale, &[&str])] = &[
        (
            Locale::En,
            &[
                "PgUp / PgDn, Ctrl-b / Ctrl-f",
                "gg / G, Home / End",
                "Ctrl-e / Ctrl-y",
                "Left / Right or h / l",
                "Switch panels by direction",
                "Ctrl-w",
                "In the filter popup: clear search and filters",
                "h switches to this help",
                "h returns to this help",
                ":help [command]",
                "[Clear]",
                "Double-click",
                "line breaks become spaces",
                "Ctrl-b is the prefix",
                "<F6>",
                "Copy and favorite act on the selected list row",
            ],
        ),
        (
            Locale::ZhCn,
            &[
                "PgUp / PgDn, Ctrl-b / Ctrl-f",
                "gg / G, Home / End",
                "Ctrl-e / Ctrl-y",
                "Left / Right 或 h / l",
                "按方向切换面板",
                "Ctrl-w",
                "筛选窗内：清空搜索与筛选",
                "按 h 切换到本帮助",
                "按 h 回到本帮助",
                ":help [命令]",
                "[清空筛选]",
                "双击",
                "换行变为空格",
                "Ctrl-b 是前缀键",
                "<F6>",
                "复制与收藏作用于列表中选中的行",
            ],
        ),
    ];
    for (locale, needles) in expected {
        // Wide enough that no key or description wraps.
        let text = help_text(*locale, "<F6>", 200);
        for needle in *needles {
            assert!(text.contains(needle), "{locale:?} lacks {needle:?}\n{text}");
        }
        assert!(!text.contains("Ctrl-u/d"), "{locale:?}");
    }
}

#[test]
fn help_lists_exactly_the_commands_the_parser_knows() {
    use super::help::Entry;
    use std::collections::BTreeSet;
    let mut known: BTreeSet<String> = super::command::COMPLETIONS
        .iter()
        .filter_map(|line| line.split_whitespace().next())
        .map(str::to_owned)
        .collect();
    known.insert("q".into());
    for locale in [crate::messages::Locale::En, crate::messages::Locale::ZhCn] {
        let title = crate::messages::text_in(locale, "help_commands_title");
        let sections = super::help::sections(locale, "");
        let section = sections
            .iter()
            .find(|section| section.title == title)
            .expect("a commands section");
        let documented: BTreeSet<String> = section
            .entries
            .iter()
            .filter_map(|entry| match entry {
                Entry::Item { keys, .. } if keys.starts_with(':') => Some(keys),
                _ => None,
            })
            .flat_map(|keys| keys.split(" / "))
            .filter_map(|variant| variant.trim_start_matches(':').split_whitespace().next())
            .map(str::to_owned)
            .collect();
        assert_eq!(documented, known, "{locale:?}");
    }
}

#[test]
fn help_stacks_keys_above_descriptions_on_narrow_screens() {
    let lines: Vec<String> = super::help::lines(crate::messages::Locale::En, "", 34)
        .iter()
        .map(|line| line.to_string())
        .collect();
    let at = lines
        .iter()
        .position(|line| line == "  Tab / Shift-Tab")
        .expect("the key stands on its own line");
    assert_eq!(lines[at + 1], "    Switch panels");
    assert!(
        !lines
            .iter()
            .any(|line| line.contains("Tab / Shift-Tab") && line.contains("Switch panels"))
    );
    // Long descriptions wrap under their own indent.
    let wrapped = lines
        .iter()
        .position(|line| line == "    Play the selected item (open")
        .expect("descriptions wrap at words");
    assert_eq!(lines[wrapped + 1], "    the list from sources)");
    // A key that must wrap hangs its continuation, and the description steps in further.
    let long = lines
        .iter()
        .position(|line| line == "  :station remove UUID")
        .expect("long keys wrap at words");
    assert_eq!(lines[long + 1], "    [--keep-references]");
    assert_eq!(lines[long + 2], "      y confirms deletion");
}

#[test]
fn help_lays_out_every_entry_at_every_width_without_losing_text() {
    use super::help::Entry;
    let squash = |text: &str| text.split_whitespace().collect::<String>();
    for locale in [crate::messages::Locale::En, crate::messages::Locale::ZhCn] {
        let sections = super::help::sections(locale, "<F6>");
        let expected: String = sections
            .iter()
            .flat_map(|section| {
                std::iter::once(squash(section.title)).chain(section.entries.iter().map(|entry| {
                    match entry {
                        Entry::Note(note) => squash(note),
                        Entry::Item { keys, detail } => squash(&format!("{keys}{detail}")),
                    }
                }))
            })
            .collect();
        for width in [12u16, 20, 34, 36, 49, 50, 52, 60, 76, 86, 120, 200] {
            let lines = super::help::lines(locale, "<F6>", width);
            for line in &lines {
                assert!(
                    super::text::width(&line.to_string()) <= usize::from(width),
                    "{locale:?} {width}: {line}"
                );
            }
            let joined = lines
                .iter()
                .map(|line| line.to_string())
                .collect::<String>();
            assert_eq!(squash(&joined), expected, "{locale:?} at {width}");
        }
    }
}

#[test]
fn help_popup_shows_its_first_and_last_lines_on_small_and_standard_terminals() {
    let data = view();
    let (sender, _receiver) = command::channel(4);
    for (columns, rows) in [(40u16, 16u16), (80, 24), (100, 24)] {
        let mut app = App::default();
        let mut terminal = Terminal::new(TestBackend::new(columns, rows)).unwrap();
        app.key(key(KeyCode::Char('?')), &data, &sender);
        terminal.draw(|f| render::draw(f, &mut app, &data)).unwrap();
        let first = text(&terminal);
        assert!(first.contains(crate::t!("help_nav_title")), "{first}");
        assert!(app.popup.total > app.popup.visible, "{columns}x{rows}");

        app.key(key(KeyCode::End), &data, &sender);
        terminal.draw(|f| render::draw(f, &mut app, &data)).unwrap();
        let last = text(&terminal);
        let inner = columns.min(88).min(columns - 2) - 2;
        let expected = super::help::lines(crate::messages::locale(), "", inner)
            .pop()
            .unwrap()
            .to_string();
        assert!(last.contains(expected.trim()), "{columns}x{rows}: {last}");
        assert!(!last.contains(crate::t!("help_nav_title")));
    }
}

#[test]
fn territory_filter_displays_names_but_selects_original_codes() {
    let mut data = view();
    Arc::make_mut(&mut data.facets)[0] = vec![crate::model::Facet {
        value: "TW".into(),
        count: 3,
    }];
    let mut app = App::default();
    let (sender, receiver) = command::channel(8);
    app.key(key(KeyCode::Char('F')), &data, &sender);
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    terminal
        .draw(|frame| render::draw(frame, &mut app, &data))
        .unwrap();
    let rendered = text(&terminal);
    assert!(rendered.contains(crate::t!("filter_country")));
    assert!(rendered.contains("TW (3)"));
    app.key(key(KeyCode::Down), &data, &sender);
    app.key(key(KeyCode::Enter), &data, &sender);
    assert_eq!(app.filter.country.as_deref(), Some("TW"));
    assert!(matches!(receiver.try_recv(), Ok(Command::Search(filter))
        if filter.country.as_deref() == Some("TW")));

    app.key(key(KeyCode::Char('F')), &data, &sender);
    for _ in 0..100 {
        app.key(key(KeyCode::Char('x')), &data, &sender);
    }
    terminal
        .draw(|frame| render::draw(frame, &mut app, &data))
        .unwrap();
    let cursor = terminal.get_cursor_position().unwrap();
    assert!(cursor.x < 75, "filter input cursor escaped its popup");
}

#[test]
fn selection_resume_restores_source_and_page_once_without_playback() {
    use crate::config::ResumeMode;
    let dir = tempfile::tempdir().unwrap();
    let config = crate::test_support::config(dir.path());
    let mut engine = Engine::new(config, Arc::new(AtomicBool::new(false))).unwrap();
    engine.config.settings.resume = ResumeMode::Selection;
    // Any attempted playback would fail to spawn this binary.
    engine.config.settings.mpv = "/missing/mpv".into();
    for i in 0..105 {
        let mut row = station(&format!("Station {i}"));
        row.clickcount = i;
        row.url = "https://example.com/live".into();
        engine.store.upsert(&row).unwrap();
        engine.store.favorite(&row.stationuuid, true).unwrap();
        if i == 1 {
            engine
                .store
                .set_meta("pref_last_station", &row.stationuuid)
                .unwrap();
        }
    }
    engine.store.set_meta("pref_source", "favorites").unwrap();
    engine.resume_last_station().unwrap();
    assert_eq!(
        engine.status.playback_state,
        crate::engine::PlaybackState::Idle
    );
    assert!(engine.status.station.is_none());
    let mut app = App::restore(&engine.store, ResumeMode::Selection).unwrap();
    assert_eq!(app.source, Source::Favorites);
    assert_eq!(app.filter.offset, 100);
    let mut data = View::new(engine.status.clone());
    data.filter = app.filter.clone();
    app.reconcile(&data); // Wait for the first query to finish.
    data.rows = engine.store.search(&app.filter).unwrap().into();
    data.rows_revision = 1;
    data.page = Some(worker::PageState {
        generation: 0,
        requested: data.filter.clone(),
        phase: worker::PagePhase::Ready,
    });
    app.reconcile(&data);
    assert_eq!(app.selected(&data).unwrap().name, "Station 1");
    // The target can disappear between restoration and the first query.
    // A later revision must not unexpectedly select it when it returns.
    let mut vanished = App::restore(&engine.store, ResumeMode::Selection).unwrap();
    let mut absent = data.clone();
    absent.rows = data
        .rows
        .iter()
        .filter(|s| s.name != "Station 1")
        .cloned()
        .collect::<Vec<_>>()
        .into();
    vanished.reconcile(&absent);
    let first = vanished.selected(&absent).unwrap().stationuuid.clone();
    data.rows_revision += 1;
    vanished.reconcile(&data);
    assert_eq!(vanished.selected(&data).unwrap().stationuuid, first);
    let (sender, _) = command::channel(8);
    app.key(key(KeyCode::Home), &data, &sender);
    data.rows_revision += 1;
    app.reconcile(&data);
    assert_eq!(app.list.selected(), Some(0));

    engine
        .store
        .set_meta("pref_last_station", "missing")
        .unwrap();
    let mut missing = App::restore(&engine.store, ResumeMode::Selection).unwrap();
    assert_eq!(missing.source, Source::Favorites);
    assert_eq!(missing.filter.offset, 0);
    data.filter = missing.filter.clone();
    data.rows = engine.store.search(&missing.filter).unwrap().into();
    missing.reconcile(&data);
    assert_eq!(missing.list.selected(), Some(0));
    let fresh = App::restore(&engine.store, ResumeMode::Disabled).unwrap();
    assert_eq!(fresh.source, Source::Stations);
    assert_eq!(fresh.filter.offset, 0);

    // The worker uses the same startup page even if preferences change
    // between App restoration and worker startup.
    let initial_filter = app.filter.clone();
    let expected_filter = initial_filter.clone();
    engine.config.settings.auto_sync_days = 0;
    engine.store.set_meta("pref_source", "stations").unwrap();
    let shared = Arc::new(Mutex::new(View::new(engine.status.clone())));
    let output = shared.clone();
    let stop = engine.stop.clone();
    let flag = stop.clone();
    let (_sender, receiver) = command::channel(8);
    let handle = thread::spawn(move || worker::run(engine, receiver, output, flag, initial_filter));
    wait_for(|| shared.lock().unwrap().rows_revision > 0);
    assert_eq!(shared.lock().unwrap().filter, expected_filter);
    stop.store(true, Ordering::Relaxed);
    handle.join().unwrap().unwrap();
}

#[test]
fn input_labels_use_display_cells_and_keep_the_cursor_inside() {
    assert_eq!(render::input_geometry("SEARCH /", 20), (11, 9));
    assert_eq!(render::input_geometry("搜索 /", 20), (13, 7));
    assert_eq!(render::input_geometry("e\u{301}", 20), (18, 2));
    assert_eq!(render::input_geometry("搜索 /", 3), (0, 2));
    assert_eq!(render::input_geometry("搜索 /", 0), (0, 0));
}

#[test]
fn pointer_motion_and_release_do_not_request_a_redraw() {
    use crossterm::event::{MouseButton, MouseEventKind};
    for kind in [
        MouseEventKind::Moved,
        MouseEventKind::Drag(MouseButton::Left),
        MouseEventKind::Up(MouseButton::Left),
    ] {
        assert!(!super::mouse_redraws(kind), "{kind:?}");
    }
    for kind in [
        MouseEventKind::Down(MouseButton::Left),
        MouseEventKind::ScrollDown,
        MouseEventKind::ScrollUp,
    ] {
        assert!(super::mouse_redraws(kind), "{kind:?}");
    }
}

#[test]
fn list_border_clicks_select_rows_without_starting_playback() {
    use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
    let data = view();
    let mut app = App::default();
    app.rects[Focus::List.index()] = ratatui::layout::Rect::new(10, 5, 40, 10);
    let (sender, receiver) = command::channel(8);
    let now = Instant::now();
    for column in [10, 49] {
        app.list.select(Some(0));
        let click = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column,
            row: 7,
            modifiers: KeyModifiers::NONE,
        };
        app.mouse_at(click, &data, &sender, now);
        assert_eq!(app.list.selected(), Some(1));
        app.mouse_at(click, &data, &sender, now + Duration::from_millis(100));
        assert!(receiver.try_recv().is_err());
    }
}

#[test]
fn double_click_plays_only_the_same_station_without_intervening_movement() {
    use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
    let data = view();
    let mut app = App::default();
    app.rects[Focus::List.index()] = ratatui::layout::Rect::new(10, 5, 40, 10);
    let (sender, receiver) = command::channel(16);
    let now = Instant::now();
    let click = MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: 15,
        row: 7,
        modifiers: KeyModifiers::NONE,
    };
    app.mouse_at(click, &data, &sender, now);
    assert_eq!(app.list.selected(), Some(1));
    assert!(receiver.try_recv().is_err());
    app.mouse_at(
        MouseEvent {
            kind: MouseEventKind::Up(MouseButton::Left),
            ..click
        },
        &data,
        &sender,
        now + Duration::from_millis(50),
    );
    app.mouse_at(click, &data, &sender, now + Duration::from_millis(100));
    assert!(matches!(receiver.try_recv(), Ok(Command::Play(id)) if id == data.rows[1].stationuuid));
    for intervening in [
        MouseEventKind::Moved,
        MouseEventKind::Drag(MouseButton::Left),
        MouseEventKind::ScrollDown,
    ] {
        app.mouse_at(click, &data, &sender, now);
        app.mouse_at(
            MouseEvent {
                kind: intervening,
                ..click
            },
            &data,
            &sender,
            now,
        );
        app.mouse_at(click, &data, &sender, now + Duration::from_millis(100));
        assert!(receiver.try_recv().is_err());
        app.mouse_at(
            MouseEvent {
                kind: MouseEventKind::Moved,
                ..click
            },
            &data,
            &sender,
            now,
        );
    }
    app.mouse_at(click, &data, &sender, now);
    app.mouse_at(click, &data, &sender, now + Duration::from_millis(501));
    assert!(receiver.try_recv().is_err());
    app.mouse_enabled = false;
    app.mouse_at(click, &data, &sender, now + Duration::from_millis(510));
    assert!(receiver.try_recv().is_err());
    app.mouse_enabled = true;
    app.help = true;
    app.mouse_at(click, &data, &sender, now + Duration::from_millis(520));
    assert!(receiver.try_recv().is_err());
    app.help = false;
    let mut changed = data.clone();
    changed.rows_revision += 1;
    app.mouse_at(click, &data, &sender, now);
    app.mouse_at(click, &changed, &sender, now + Duration::from_millis(100));
    assert!(receiver.try_recv().is_err());
    app.source = Source::Recordings;
    app.mouse_at(click, &data, &sender, now);
    app.mouse_at(click, &data, &sender, now + Duration::from_millis(100));
    assert!(receiver.try_recv().is_err());
}

#[test]
fn context_hints_match_source_and_focus_actions() {
    let data = view();
    let (sender, receiver) = command::channel(20);
    for source in Source::ALL {
        let mut app = App::default();
        app.source = source;
        for focus in Focus::ALL {
            let hint = render::context_hint(source, focus);
            assert!(!hint.is_empty());
            if focus == Focus::List {
                let station_actions = source != Source::Recordings;
                assert!(hint.contains("Enter play"));
                assert_eq!(hint.contains("f favorite"), station_actions);
                app.focus = focus;
                app.key(key(KeyCode::Enter), &data, &sender);
                assert_eq!(
                    matches!(receiver.try_recv(), Ok(Command::Play(_))),
                    station_actions
                );
                app.key(key(KeyCode::Char('f')), &data, &sender);
                assert_eq!(
                    matches!(receiver.try_recv(), Ok(Command::ToggleFavorite(_))),
                    station_actions
                );
            }
        }
        app.focus = Focus::List;
        app.message.clear();
        let mut terminal = Terminal::new(TestBackend::new(160, 48)).unwrap();
        terminal
            .draw(|frame| render::draw(frame, &mut app, &data))
            .unwrap();
        assert!(text(&terminal).contains(render::context_hint(source, Focus::List)));
    }
}

#[test]
fn bottom_shortcuts_highlight_keys_and_disambiguate_source_keys() {
    use crate::config::Theme;
    let data = view();
    let mut app = App::default();
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    terminal
        .draw(|frame| render::draw(frame, &mut app, &data))
        .unwrap();
    let rendered = text(&terminal);
    assert!(rendered.contains("j/k select"));
    assert!(rendered.contains("[ / ] source"));
    assert!(!rendered.contains('`'));
    let buffer = terminal.backend().buffer();
    assert_eq!(buffer[(0, 22)].fg, Color::LightBlue);
    assert_eq!(buffer[(4, 22)].fg, Color::Reset);
    let source_keys = (1..80 - 5).find(|&x| {
        buffer[(x - 1, 23)].symbol() == " "
            && buffer[(x, 23)].symbol() == "["
            && buffer[(x, 23)].fg == Color::LightBlue
            && buffer[(x + 1, 23)].symbol() == " "
            && buffer[(x + 2, 23)].symbol() == "/"
            && buffer[(x + 2, 23)].fg == Color::Reset
            && buffer[(x + 3, 23)].symbol() == " "
            && buffer[(x + 4, 23)].symbol() == "]"
            && buffer[(x + 4, 23)].fg == Color::LightBlue
            && buffer[(x + 5, 23)].symbol() == " "
            && buffer[(x + 5, 23)].fg == Color::Reset
    });
    assert!(source_keys.is_some(), "{rendered}");

    app.theme = Theme::Mono;
    terminal
        .draw(|frame| render::draw(frame, &mut app, &data))
        .unwrap();
    let mono = text(&terminal);
    assert_eq!(mono, rendered);
    for cell in &terminal.backend().buffer().content {
        assert_eq!((cell.fg, cell.bg), (Color::Reset, Color::Reset));
    }
}

fn assert_shortcut_color(
    terminal: &Terminal<TestBackend>,
    rect: ratatui::layout::Rect,
    key: &str,
    color: Color,
) {
    let buffer = terminal.backend().buffer();
    let chars: Vec<_> = key.chars().map(|c| c.to_string()).collect();
    let mut found = false;
    for y in rect.y..rect.bottom() {
        for x in rect.x..rect.right().saturating_sub(chars.len() as u16) {
            if (x == rect.x || matches!(buffer[(x - 1, y)].symbol(), " " | "│" | "┃"))
                && chars
                    .iter()
                    .enumerate()
                    .all(|(i, c)| buffer[(x + i as u16, y)].symbol() == c)
                && buffer[(x + chars.len() as u16, y)].symbol() == " "
            {
                found = true;
                for i in 0..chars.len() as u16 {
                    assert_eq!(buffer[(x + i, y)].fg, color, "{key} at {x},{y}");
                }
            }
        }
    }
    assert!(found, "missing {key}: {}", text(terminal));
}

#[test]
fn localized_shortcut_rows_fit_their_terminal_widths() {
    use crate::messages::{Locale, text_in};
    use unicode_width::UnicodeWidthStr;
    for locale in [Locale::En, Locale::ZhCn] {
        for (key, width) in [
            ("keys_compact", 40),
            ("keys_global", 80),
            ("keys_recordings", 80),
        ] {
            let text = text_in(locale, key).replace('`', "");
            assert!(text.width() <= width, "{locale:?} {key}: {text}");
        }
    }
}

#[test]
fn shortcut_styles_cover_compact_recordings_and_source_hints_in_each_theme() {
    use crate::config::Theme;
    let data = view();
    for (theme, color) in [
        (Theme::Default, Color::LightBlue),
        (Theme::Light, Color::Blue),
        (Theme::Mono, Color::Reset),
    ] {
        for width in [60, 80, 100] {
            let mut app = App::default();
            app.theme = theme;
            let mut terminal = Terminal::new(TestBackend::new(width, 24)).unwrap();
            let bottom = ratatui::layout::Rect::new(0, 23, width, 1);
            for source in [Source::Stations, Source::Recordings] {
                app.source = source;
                terminal.draw(|f| render::draw(f, &mut app, &data)).unwrap();
                assert_shortcut_color(
                    &terminal,
                    bottom,
                    if width < 80 { "1-5" } else { "Tab/1-5" },
                    color,
                );
                assert_shortcut_color(&terminal, bottom, ":", color);
                assert_shortcut_color(&terminal, bottom, "q", color);
                if width >= 80 {
                    assert_shortcut_color(&terminal, bottom, "[", color);
                    assert_shortcut_color(&terminal, bottom, "]", color);
                }
                if width >= 100 {
                    for key in ["j/k", "Enter", "[", "]"] {
                        assert_shortcut_color(
                            &terminal,
                            app.rects[Focus::Sources.index()],
                            key,
                            color,
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn modal_and_editor_shortcuts_share_the_theme_key_style() {
    use crate::config::Theme;
    let data = view();
    let (sender, _receiver) = command::channel(16);
    for (theme, color) in [
        (Theme::Default, Color::LightBlue),
        (Theme::Light, Color::Blue),
        (Theme::Mono, Color::Reset),
    ] {
        for width in [60, 100] {
            let mut terminal = Terminal::new(TestBackend::new(width, 24)).unwrap();
            let popup = ratatui::layout::Rect::new(0, 0, width, 21);
            let bottom = ratatui::layout::Rect::new(0, 23, width, 1);
            let mut app = App::default();
            app.theme = theme;
            app.key(key(KeyCode::Char('F')), &data, &sender);
            terminal.draw(|f| render::draw(f, &mut app, &data)).unwrap();
            for key in ["Tab", "Enter", "Esc"] {
                assert_shortcut_color(&terminal, popup, key, color);
                assert_shortcut_color(&terminal, bottom, key, color);
            }
            assert_shortcut_color(&terminal, popup, "Ctrl-U", color);
            app.key(key(KeyCode::Esc), &data, &sender);
            for code in [':', '/'] {
                app.key(key(KeyCode::Char(code)), &data, &sender);
                terminal.draw(|f| render::draw(f, &mut app, &data)).unwrap();
                let hint = ratatui::layout::Rect::new(0, 22, width, 2);
                assert_shortcut_color(&terminal, hint, "Enter", color);
                assert_shortcut_color(&terminal, hint, "Esc", color);
                // Diagnostic text is ordinary text, even when it names a key.
                app.message = "Enter failed".into();
                terminal.draw(|f| render::draw(f, &mut app, &data)).unwrap();
                let expected = if theme == Theme::Default {
                    Color::Yellow
                } else if theme == Theme::Light {
                    Color::Magenta
                } else {
                    Color::Reset
                };
                assert_shortcut_color(&terminal, hint, "Enter", expected);
                app.message.clear();
                app.key(key(KeyCode::Esc), &data, &sender);
            }
            app.key(key(KeyCode::Char('?')), &data, &sender);
            terminal.draw(|f| render::draw(f, &mut app, &data)).unwrap();
            assert_shortcut_color(&terminal, bottom, "Esc/?/q", color);
            app.key(key(KeyCode::Esc), &data, &sender);
            app.key(key(KeyCode::Char(':')), &data, &sender);
            app.editor
                .set(&format!("station remove {}", data.rows[0].stationuuid));
            app.key(key(KeyCode::Enter), &data, &sender);
            terminal.draw(|f| render::draw(f, &mut app, &data)).unwrap();
            for key in ["y", "Enter/n/Esc/q"] {
                assert_shortcut_color(&terminal, popup, key, color);
                assert_shortcut_color(&terminal, bottom, key, color);
            }
        }
    }
}

#[test]
fn station_removal_requires_a_second_confirmation_in_tui() {
    let data = view();
    let (sender, receiver) = command::channel(10);
    let mut app = App::default();
    for confirm in [false, true] {
        app.key(key(KeyCode::Char(':')), &data, &sender);
        app.editor
            .set(&format!("station remove {}", data.rows[0].stationuuid));
        app.key(key(KeyCode::Enter), &data, &sender);
        assert!(app.confirmation.is_some());
        assert!(receiver.try_recv().is_err());
        app.key(
            key(if confirm {
                KeyCode::Char('y')
            } else {
                KeyCode::Esc
            }),
            &data,
            &sender,
        );
        assert!(app.confirmation.is_none());
        assert_eq!(
            matches!(
                receiver.try_recv(),
                Ok(Command::Station(crate::cli::StationAction::Remove { .. }))
            ),
            confirm
        );
    }
}

#[test]
fn source_preferences_are_persisted_by_the_owner_and_restored_when_enabled() {
    let dir = tempfile::tempdir().unwrap();
    let config = crate::test_support::config(dir.path());
    let mut engine = Engine::new(config, Arc::new(AtomicBool::new(false))).unwrap();
    let data = Arc::new(Mutex::new(View::new(engine.status.clone())));
    let mut filter = Filter {
        limit: 100,
        ..Filter::default()
    };
    let mut job = None;
    for source in Source::ALL {
        let value = Filter {
            geo: (source == Source::Explore)
                .then_some(crate::geo::GeoFilter::Cell(crate::geo::GeoBounds::default())),
            favorites: source == Source::Favorites,
            history: source == Source::History,
            limit: 100,
            ..Filter::default()
        };
        let command = if source == Source::Recordings {
            Command::Recordings(value)
        } else {
            Command::Search(value)
        };
        worker::execute(command, &mut engine, &mut filter, &mut job, &data).unwrap();
        assert_eq!(
            Source::restore(&engine.store, crate::config::ResumeMode::Playback).unwrap(),
            source
        );
        assert_eq!(
            Source::restore(&engine.store, crate::config::ResumeMode::Disabled).unwrap(),
            Source::Stations
        );
    }
}

#[test]
fn filter_popup_selects_local_candidates_with_the_keyboard() {
    let mut data = view();
    Arc::make_mut(&mut data.facets)[0] = vec![
        crate::model::Facet {
            value: "CN".into(),
            count: 2,
        },
        crate::model::Facet {
            value: "FR".into(),
            count: 3,
        },
    ];
    let (sender, receiver) = command::channel(10);
    let mut app = App::default();
    app.key(key(KeyCode::Char('F')), &data, &sender);
    app.request_facets(&sender);
    assert!(matches!(
        receiver.try_recv(),
        Ok(Command::Facets { selected: 0, .. })
    ));
    app.key(key(KeyCode::Down), &data, &sender);
    app.key(key(KeyCode::Down), &data, &sender);
    assert_eq!(app.filter_edit.as_ref().unwrap().1[0], "FR");
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    terminal
        .draw(|frame| render::draw(frame, &mut app, &data))
        .unwrap();
    assert!(text(&terminal).contains("FR (3)"));
    app.key(key(KeyCode::Enter), &data, &sender);
    assert_eq!(app.filter.country.as_deref(), Some("FR"));
    assert!(
        matches!(receiver.try_recv(), Ok(Command::Search(filter)) if filter.country.as_deref() == Some("FR"))
    );
}

#[test]
fn clear_filters_keeps_source_and_pagination_size() {
    use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
    let data = view();
    for mouse in [false, true] {
        let (sender, receiver) = command::channel(10);
        let mut app = App::default();
        app.source = Source::Favorites;
        app.filter = Filter {
            query: "jazz".into(),
            country: Some("CN".into()),
            language: Some("zh".into()),
            tag: Some("public".into()),
            custom: true,
            min_bitrate: 128,
            favorites: true,
            limit: 25,
            offset: 25,
            ..Filter::default()
        };
        if mouse {
            let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
            terminal
                .draw(|frame| render::draw(frame, &mut app, &data))
                .unwrap();
            app.mouse(
                MouseEvent {
                    kind: MouseEventKind::Down(MouseButton::Left),
                    column: app.filter_summary.x,
                    row: app.filter_summary.y,
                    modifiers: KeyModifiers::NONE,
                },
                &data,
                &sender,
            );
        } else {
            app.key(key(KeyCode::Char('F')), &data, &sender);
            app.request_facets(&sender);
            receiver.try_recv().unwrap();
            app.key(
                KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL),
                &data,
                &sender,
            );
        }
        assert_eq!(
            app.filter,
            Filter {
                favorites: true,
                limit: 25,
                ..Filter::default()
            }
        );
        assert!(app.filter_edit.is_none());
        assert!(matches!(receiver.try_recv(), Ok(Command::Search(_))));
    }
}

#[test]
fn expired_catalog_syncs_at_startup_and_can_be_disabled() {
    let dir = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let api = format!("http://{}", listener.local_addr().unwrap());
    let config = crate::test_support::config_with_api(dir.path(), &api);
    let stop = Arc::new(AtomicBool::new(false));
    let engine = Engine::new(config, stop.clone()).unwrap();
    assert!(!worker::auto_sync_due(&engine.store, 7).unwrap());
    let old = station("Old");
    engine.store.upsert(&old).unwrap();
    engine
        .store
        .set_meta(
            "last_sync",
            &(chrono::Utc::now() - chrono::Duration::days(8)).to_rfc3339(),
        )
        .unwrap();
    assert!(worker::auto_sync_due(&engine.store, 7).unwrap());
    assert!(!worker::auto_sync_due(&engine.store, 0).unwrap());
    assert!(!worker::auto_sync_due(&engine.store, 9).unwrap());
    let replacement = station("Automatically synced");
    let body = serde_json::to_vec(&vec![replacement]).unwrap();
    let server = thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Ok((mut stream, _)) = listener.accept() {
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let mut request = [0; 8192];
                let _ = stream.read(&mut request);
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                )
                .unwrap();
                stream.write_all(&body).unwrap();
                break;
            }
            assert!(
                Instant::now() < deadline,
                "automatic sync never requested the catalog"
            );
            thread::sleep(Duration::from_millis(10));
        }
    });
    let shared = Arc::new(Mutex::new(View::new(engine.status.clone())));
    let worker_view = shared.clone();
    let worker_stop = stop.clone();
    let (_sender, receiver) = command::channel(10);
    let initial_filter = App::restore(&engine.store, engine.config.settings.resume)
        .unwrap()
        .filter;
    let handle = thread::spawn(move || {
        worker::run(engine, receiver, worker_view, worker_stop, initial_filter)
    });
    let deadline = Instant::now() + Duration::from_secs(5);
    while shared
        .lock()
        .unwrap()
        .rows
        .first()
        .is_none_or(|s| s.name != "Automatically synced")
    {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(20));
    }
    stop.store(true, Ordering::Relaxed);
    handle.join().unwrap().unwrap();
    server.join().unwrap();
}

#[test]
fn commands_use_cli_arguments_and_context_without_shell_execution() {
    let data = view();
    let filter = Filter {
        country: Some("CN".into()),
        ..Filter::default()
    };
    let selected = data.rows.first();
    let current = data.status.station.as_ref();
    assert!(matches!(
        command::parse("sync", selected, current, &filter).unwrap(),
        Parsed::Worker(Command::Sync(false))
    ));
    match command::parse("station add '中文 电台' 'https://example.com/live?a=1&b=2' --backup-url https://example.com/backup",selected,current,&filter).unwrap(){
        Parsed::Worker(Command::Add{name,url,backups})=>{assert_eq!(name,"中文 电台");assert!(url.contains("&b=2"));assert_eq!(backups.len(),1);},_=>panic!()}
    match command::parse(
        "search 'public radio' --country FR --min-bitrate 192",
        selected,
        current,
        &filter,
    )
    .unwrap()
    {
        Parsed::Search(filter) => {
            assert_eq!(filter.query, "public radio");
            assert_eq!(filter.min_bitrate, 192);
            assert_eq!(filter.country.as_deref(), Some("FR"));
        }
        _ => panic!(),
    }
    match command::parse(
        "record --duration 30m --output-dir 'my music'",
        selected,
        current,
        &filter,
    )
    .unwrap()
    {
        Parsed::Worker(Command::Record {
            uuid,
            duration,
            dir,
        }) => {
            assert!(uuid.is_none());
            assert_eq!(duration.unwrap().as_secs(), 1800);
            assert_eq!(dir.unwrap().to_str(), Some("my music"));
        }
        _ => panic!(),
    }
    match command::parse("play", selected, current, &filter).unwrap() {
        Parsed::Worker(Command::Play(uuid)) => assert_eq!(uuid, selected.unwrap().stationuuid),
        _ => panic!(),
    }
    match command::parse("current", selected, current, &filter).unwrap() {
        Parsed::Worker(Command::Info(uuid, _)) => assert_eq!(uuid, current.unwrap().stationuuid),
        _ => panic!(),
    }
    assert!(matches!(
        command::parse("record stop", None, None, &filter).unwrap(),
        Parsed::Worker(Command::RecordStop)
    ));
    for invalid in [
        "sync extra",
        "sync --data-dir /tmp",
        "sync --lang en",
        "volume 101",
        "search 'unclosed",
        "!echo test",
        "quit extra",
        "focus nowhere",
        "record stop extra",
    ] {
        assert!(
            command::parse(invalid, selected, current, &filter).is_err(),
            "{invalid}"
        );
    }
    assert!(command::parse("play", None, None, &filter).is_err());
}

#[test]
fn command_errors_come_from_the_message_catalog() {
    let filter = Filter::default();
    let error = |line: &str| {
        command::parse(line, None, None, &filter)
            .err()
            .unwrap()
            .to_string()
    };
    assert_eq!(error(""), crate::t!("command_empty"));
    assert_eq!(error("play"), crate::t!("command_need_station"));
    assert_eq!(error("current"), crate::t!("command_nothing_playing"));
    assert_eq!(error("record"), crate::t!("command_nothing_playing_uuid"));
    assert_eq!(error("set mouse maybe"), crate::t!("command_usage_set"));
    assert_eq!(error("volume 101"), crate::t!("command_usage_volume"));
    assert_eq!(error("focus nowhere"), crate::t!("command_usage_focus"));
    assert_eq!(
        error("help nosuch"),
        crate::t!("command_unknown_help", "word" => "nosuch")
    );
    assert_eq!(error("sync --json"), crate::t!("command_process_options"));
    assert_eq!(error("history --all"), crate::t!("command_paging"));
}

#[test]
fn editor_handles_unicode_cursor_completion_and_history_draft() {
    let mut editor = Editor::default();
    editor.begin("上海FM");
    editor.key(key(KeyCode::Home), true);
    editor.key(key(KeyCode::Right), true);
    editor.key(key(KeyCode::Char('好')), true);
    assert_eq!(editor.text, "上好海FM");
    editor.key(key(KeyCode::Delete), true);
    assert_eq!(editor.text, "上好FM");
    editor.key(key(KeyCode::Backspace), true);
    assert_eq!(editor.text, "上FM");
    editor.set("sy");
    editor.key(key(KeyCode::Tab), true);
    assert_eq!(editor.text, "sync");
    editor.key(key(KeyCode::Tab), true);
    assert_eq!(editor.text, "sync-cancel");
    editor.key(key(KeyCode::BackTab), true);
    assert_eq!(editor.text, "sync");
    editor.remember().unwrap();
    editor.begin("draft");
    editor.key(key(KeyCode::Up), true);
    assert_eq!(editor.text, "sync");
    editor.key(key(KeyCode::Down), true);
    assert_eq!(editor.text, "draft");
    editor.begin("search --cou");
    editor.key(key(KeyCode::Tab), true);
    assert_eq!(editor.text, "search --country");
}

#[test]
fn modes_route_text_without_triggering_normal_actions_and_restore_search() {
    let data = view();
    let (sender, receiver) = command::channel(16);
    let mut app = App::default();
    app.key(key(KeyCode::Char(':')), &data, &sender);
    for c in "station add 'q r n' https://example.com/live".chars() {
        app.key(key(KeyCode::Char(c)), &data, &sender);
    }
    assert_eq!(app.mode, Mode::Command);
    assert!(!app.quit);
    assert!(receiver.try_recv().is_err());
    app.key(key(KeyCode::Enter), &data, &sender);
    assert_eq!(app.mode, Mode::Normal);
    assert!(matches!(receiver.try_recv().unwrap(), Command::Add { .. }));
    app.key(key(KeyCode::Char(':')), &data, &sender);
    app.editor.set("volume 999");
    app.key(key(KeyCode::Enter), &data, &sender);
    assert_eq!(app.mode, Mode::Command);
    assert!(app.message.contains("0–100"));
    app.key(key(KeyCode::Esc), &data, &sender);
    app.filter.query = "old".into();
    app.key(key(KeyCode::Char('/')), &data, &sender);
    app.key(key(KeyCode::Char('新')), &data, &sender);
    app.flush_search(&sender);
    assert_eq!(app.filter.query, "old新");
    app.key(key(KeyCode::Esc), &data, &sender);
    assert_eq!(app.filter.query, "old");
    app.flush_search(&sender);
    let searches: Vec<_> = receiver
        .try_iter()
        .filter_map(|c| {
            if let Command::Search(filter) = c {
                Some(filter.query)
            } else {
                None
            }
        })
        .collect();
    assert_eq!(searches, vec!["old新", "old"]);
    app.key(key(KeyCode::Tab), &data, &sender);
    assert_eq!(app.focus, Focus::Details);
    app.key(key(KeyCode::BackTab), &data, &sender);
    assert_eq!(app.focus, Focus::List);
    app.key(key(KeyCode::Char('4')), &data, &sender);
    assert_eq!(app.focus, Focus::Player);
    app.key(key(KeyCode::Char(']')), &data, &sender);
    assert_eq!(app.source, Source::Favorites);
}

#[test]
fn renderer_marks_focus_and_distinguishes_current_from_selected_station() {
    let mut data = view();
    let mut app = App::default();
    let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
    terminal
        .draw(|frame| render::draw(frame, &mut app, &data))
        .unwrap();
    let output = text(&terminal);
    assert!(output.contains("Current Station"));
    assert!(output.contains("Selected Station"));
    assert!(output.contains("192 kbps"));
    assert!(output.contains("96 kbps"));
    assert!(output.contains("AAC"));
    let rect = app.rects[Focus::List.index()];
    let cell = &terminal.backend().buffer()[(rect.x, rect.y)];
    assert_eq!(cell.fg, Color::Cyan);
    assert_eq!(cell.symbol(), "┏");
    let player = app.rects[Focus::Player.index()];
    assert_ne!(
        terminal.backend().buffer()[(player.x, player.y)].fg,
        Color::Cyan
    );
    app.focus = Focus::Player;
    terminal
        .draw(|frame| render::draw(frame, &mut app, &data))
        .unwrap();
    assert_eq!(
        terminal.backend().buffer()[(player.x, player.y)].fg,
        Color::Cyan
    );
    // Explicit current-station inspection also works from the recordings source.
    app.source = Source::Recordings;
    app.inspect_info = true;
    app.focus = Focus::Details;
    data.inspected = data.status.station.clone();
    app.details_scroll = 4096;
    terminal
        .draw(|frame| render::draw(frame, &mut app, &data))
        .unwrap();
    assert!(app.details_scroll < 4096);
    assert!(text(&terminal).contains(crate::t!("current_request_url", "url" => "").trim()));
    let details = app.rects[Focus::Details.index()];
    let detail_text = (details.y + 1..details.bottom() - 1)
        .map(|y| {
            (details.x + 1..details.right() - 1)
                .map(|x| terminal.backend().buffer()[(x, y)].symbol())
                .collect::<String>()
                .trim_end()
                .to_owned()
        })
        .collect::<String>();
    assert!(detail_text.contains("https://example.com/backup"));
    app.help = true;
    app.help_scroll = u16::MAX;
    terminal
        .draw(|frame| render::draw(frame, &mut app, &data))
        .unwrap();
    assert!(text(&terminal).contains("Radio Browser"));
    app.help = false;
    data.status.station.as_mut().unwrap().bitrate = 0;
    data.status.station.as_mut().unwrap().codec = "UNKNOWN".into();
    for (width, height) in [(40, 12), (60, 18), (90, 24), (120, 30), (20, 8)] {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        app.mode = Mode::Command;
        app.editor.set(&"中文电台".repeat(20));
        terminal
            .draw(|frame| render::draw(frame, &mut app, &data))
            .unwrap();
        if width >= 100 && height >= 20 {
            assert!(text(&terminal).contains(crate::t!("bitrate_unknown")));
        } else if width >= 40 && height >= 16 {
            assert!(text(&terminal).contains("Current Station"));
        }
    }
}

#[test]
fn focus_navigation_mouse_and_stale_results_follow_visible_panels() {
    use crossterm::event::{MouseEvent, MouseEventKind};
    let mut data = view();
    let mut app = App::default();
    let (sender, receiver) = command::channel(16);
    let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
    terminal
        .draw(|frame| render::draw(frame, &mut app, &data))
        .unwrap();
    app.key(
        KeyEvent::new(KeyCode::Char('l'), KeyModifiers::CONTROL),
        &data,
        &sender,
    );
    assert_eq!(app.focus, Focus::Details);
    app.key(
        KeyEvent::new(KeyCode::Char('k'), KeyModifiers::CONTROL),
        &data,
        &sender,
    );
    assert_eq!(app.focus, Focus::Player);
    app.focus = Focus::List;
    app.reconcile(&data);
    let selected_id = app.selected(&data).unwrap().stationuuid.clone();
    Arc::make_mut(&mut data.rows).swap(0, 1);
    data.rows_revision += 1;
    app.reconcile(&data);
    assert_eq!(app.selected(&data).unwrap().stationuuid, selected_id);
    app.filter.query = "pending".into();
    app.key(key(KeyCode::Enter), &data, &sender);
    assert!(receiver.try_recv().is_err());
    assert!(app.selected(&data).is_none());
    app.filter = data.filter.clone();
    // On a short terminal details share the list area: mouse wheel stays there.
    let mut terminal = Terminal::new(TestBackend::new(60, 16)).unwrap();
    app.focus = Focus::Details;
    terminal
        .draw(|frame| render::draw(frame, &mut app, &data))
        .unwrap();
    let rect = app.rects[Focus::Details.index()];
    app.mouse(
        MouseEvent {
            kind: MouseEventKind::ScrollDown,
            column: rect.x + 1,
            row: rect.y + 1,
            modifiers: KeyModifiers::NONE,
        },
        &data,
        &sender,
    );
    assert_eq!(app.focus, Focus::Details);
    assert_eq!(app.details_scroll, 3);
}

#[test]
fn background_sync_keeps_commands_responsive_cancels_atomically_and_refreshes() {
    let dir = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let api = format!("http://{}", listener.local_addr().unwrap());
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let old = station("Old station");
    let replacement = station("New station");
    let page = serde_json::to_vec(&vec![replacement.clone(); 1000]).unwrap();
    let tail = serde_json::to_vec(&vec![replacement.clone()]).unwrap();
    let server = thread::spawn(move || {
        for index in 0..3 {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut buf = [0; 8192];
            let _ = stream.read(&mut buf);
            if index == 1 {
                started_tx.send(()).unwrap();
                release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
            }
            let body = if index == 0 { &page } else { &tail };
            let header = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(header.as_bytes());
            let _ = stream.write_all(body);
        }
    });
    let config = crate::test_support::config_with_api(dir.path(), &api);
    let store = Store::open(&config.data_dir.join("radio.db")).unwrap();
    store.upsert(&old).unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let engine = Engine::new(config, stop.clone()).unwrap();
    let view = Arc::new(Mutex::new(View::new(engine.status.clone())));
    let (sender, receiver) = command::channel(16);
    let worker_view = view.clone();
    let worker_stop = stop.clone();
    let initial_filter = App::restore(&engine.store, engine.config.settings.resume)
        .unwrap()
        .filter;
    let handle = thread::spawn(move || {
        worker::run(engine, receiver, worker_view, worker_stop, initial_filter)
    });
    sender.send(Command::Sync(false)).unwrap();
    started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    sender.send(Command::Sync(false)).unwrap();
    sender.send(Command::Volume(37)).unwrap();
    wait_for(|| view.lock().unwrap().status.volume == 37);
    assert!(view.lock().unwrap().task_active);
    assert!(
        view.lock()
            .unwrap()
            .logs
            .iter()
            .any(|s| s.contains(crate::t!("catalog_task_busy")))
    );
    sender.send(Command::Cancel).unwrap();
    wait_for(|| view.lock().unwrap().task == crate::t!("catalog_task_cancelling"));
    release_tx.send(()).unwrap();
    wait_for(|| !view.lock().unwrap().task_active);
    assert!(store.meta("last_sync").unwrap().is_none());
    assert!(store.station(&old.stationuuid).unwrap().active);
    sender.send(Command::Sync(false)).unwrap();
    wait_for(|| store.meta("last_sync").unwrap().is_some());
    wait_for(|| {
        view.lock()
            .unwrap()
            .rows
            .iter()
            .any(|s| s.stationuuid == replacement.stationuuid)
    });
    assert!(!store.station(&old.stationuuid).unwrap().active);
    {
        let data = view.lock().unwrap();
        assert_eq!(data.task, crate::t!("catalog_sync_done", "count" => 1));
        assert_no_cjk(&data.task);
        assert_no_cjk(&data.operation_error);
        for line in data.logs.iter() {
            assert_no_cjk(line);
        }
    }
    stop.store(true, Ordering::Relaxed);
    drop(sender);
    handle.join().unwrap().unwrap();
    server.join().unwrap();
}
fn wait_for(mut predicate: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !predicate() {
        assert!(Instant::now() < deadline, "timed out waiting for state");
        thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn ctrl_w_keeps_unicode_boundaries() {
    for (input, expected) in [
        ("one two", "one "),
        ("甲　乙", "甲　"),
        ("甲\u{a0}乙", "甲\u{a0}"),
        ("one  two  ", "one  "),
        ("中文", ""),
        ("a e\u{301}", "a "),
        ("", ""),
    ] {
        let mut editor = Editor::default();
        editor.set(input);
        editor.key(
            KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL),
            false,
        );
        assert_eq!(editor.text, expected);
        assert!(editor.text.is_char_boundary(editor.cursor));
    }
    let mut editor = Editor::default();
    editor.set("甲　乙丙");
    editor.key(key(KeyCode::Left), false);
    editor.key(
        KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL),
        false,
    );
    assert_eq!(editor.text, "甲　丙");
    assert_eq!(editor.cursor, "甲　".len());
}

#[test]
fn ctrl_c_quits_every_mode_and_small_screen_q_quits() {
    let data = view();
    let (tx, _rx) = command::channel(16);
    for mode in [Mode::Normal, Mode::Search, Mode::Command] {
        for popup in 0..3 {
            let mut app = App::default();
            app.mode = mode;
            app.help = popup == 1;
            if popup == 2 {
                app.filter_edit = Some((0, Default::default()));
            }
            app.key(
                KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
                &data,
                &tx,
            );
            assert!(app.quit);
        }
    }
    let mut app = App::default();
    app.help = true;
    let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
    terminal.draw(|f| render::draw(f, &mut app, &data)).unwrap();
    // Input observes a shrink before the next draw hides the popup.
    app.update_size(20, 10);
    app.key(key(KeyCode::Char('q')), &data, &tx);
    assert!(app.quit);
    app.quit = false;
    let mut terminal = Terminal::new(TestBackend::new(20, 10)).unwrap();
    terminal.draw(|f| render::draw(f, &mut app, &data)).unwrap();
    // Growing again restores the help popup's ordinary q behavior.
    app.update_size(100, 24);
    app.key(key(KeyCode::Char('q')), &data, &tx);
    assert!(!app.quit);
    assert!(!app.help);
}

#[test]
fn rapid_shortcuts_apply_against_owner_state() {
    let dir = tempfile::tempdir().unwrap();
    let config = crate::test_support::config(dir.path());
    let mut engine = Engine::new(config, Arc::new(AtomicBool::new(false))).unwrap();
    engine.status.volume = 80;
    let frozen = view();
    engine.store.upsert(&frozen.rows[0]).unwrap();
    let shared = Arc::new(Mutex::new(frozen.clone()));
    let (tx, rx) = command::channel(16);
    let mut app = App::default();
    for c in ['+', '+', '+', 'f', 'f'] {
        app.key(key(KeyCode::Char(c)), &frozen, &tx);
    }
    let mut filter = app.filter.clone();
    let mut job = None;
    for command in rx.try_iter() {
        worker::execute(command, &mut engine, &mut filter, &mut job, &shared).unwrap();
    }
    assert_eq!(engine.status.volume, 95);
    assert!(
        !engine
            .store
            .station(&frozen.rows[0].stationuuid)
            .unwrap()
            .favorite
    );
}

#[test]
fn full_queue_retries_every_view_query_with_latest_filter() {
    let mut data = view();
    Arc::make_mut(&mut data.rows).resize(100, station("row"));
    data.total = 200;
    for action in 0..5 {
        let (tx, rx) = command::channel(1);
        tx.send(Command::Search(data.filter.clone())).unwrap();
        let mut app = App::default();
        match action {
            0 => app.key(key(KeyCode::Char(']')), &data, &tx),
            1 => app.key(key(KeyCode::PageDown), &data, &tx),
            2 => {
                app.key(key(KeyCode::Char(':')), &data, &tx);
                app.editor.set("search jazz");
                app.key(key(KeyCode::Enter), &data, &tx);
            }
            3 => {
                app.key(key(KeyCode::Char('F')), &data, &tx);
                app.key(key(KeyCode::Enter), &data, &tx);
            }
            _ => {
                for _ in 0..3 {
                    app.key(key(KeyCode::Char(']')), &data, &tx);
                }
            }
        }
        assert!(app.search_due.is_some(), "action {action}");
        rx.recv().unwrap();
        app.flush_search(&tx);
        assert!(app.search_due.is_none());
        match rx.recv().unwrap() {
            Command::Search(filter) => assert_eq!(filter, app.filter),
            Command::Recordings(_) => assert_eq!(app.source, Source::Recordings),
            _ => panic!("expected retryable query"),
        }
    }
}

#[test]
fn failed_info_replaces_old_details_and_external_edits_refresh() {
    let dir = tempfile::tempdir().unwrap();
    let config = crate::test_support::config(dir.path());
    let mut engine = Engine::new(config, Arc::new(AtomicBool::new(false))).unwrap();
    let row = station("old detail");
    engine.store.upsert(&row).unwrap();
    let shared = Arc::new(Mutex::new(View::new(engine.status.clone())));
    let mut filter = Filter {
        limit: 100,
        ..Filter::default()
    };
    let mut job = None;
    worker::execute(
        Command::Info(row.stationuuid.clone(), 1),
        &mut engine,
        &mut filter,
        &mut job,
        &shared,
    )
    .unwrap();
    assert!(shared.lock().unwrap().inspected.is_some());
    worker::execute(
        Command::Info("missing".into(), 2),
        &mut engine,
        &mut filter,
        &mut job,
        &shared,
    )
    .unwrap();
    let data = shared.lock().unwrap().clone();
    assert!(data.inspected.is_none());
    assert_eq!(data.inspected_request, 2);
    assert!(data.inspected_error.is_some());
    let mut app = App::default();
    app.inspect_info = true;
    app.info_request = 2;
    app.focus = Focus::Details;
    let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
    terminal.draw(|f| render::draw(f, &mut app, &data)).unwrap();
    assert!(text(&terminal).contains("Details query failed"));
    assert_no_cjk(&text(&terminal));
    assert!(!text(&terminal).contains("old detail"));
    let before = engine.store.revision().unwrap();
    let external = Store::open(&dir.path().join("radio.db")).unwrap();
    external.favorite(&row.stationuuid, true).unwrap();
    assert_ne!(engine.store.revision().unwrap(), before);
    worker::execute(
        Command::Refresh,
        &mut engine,
        &mut filter,
        &mut job,
        &shared,
    )
    .unwrap();
    worker::refresh_if_needed(&engine, &filter, &shared, false).unwrap();
    assert!(shared.lock().unwrap().rows[0].favorite);
}

#[test]
fn tui_recording_rejects_a_different_station() {
    let dir = tempfile::tempdir().unwrap();
    let config = crate::test_support::config(dir.path());
    let mut engine = Engine::new(config, Arc::new(AtomicBool::new(false))).unwrap();
    let a = station("A");
    let b = station("B");
    engine.status.station = Some(a);
    engine.status.playback_state = crate::engine::PlaybackState::Playing;
    let shared = Arc::new(Mutex::new(View::new(engine.status.clone())));
    let mut filter = Filter {
        limit: 100,
        ..Filter::default()
    };
    let mut job = None;
    let error = worker::execute(
        Command::Record {
            uuid: Some(b.stationuuid),
            duration: None,
            dir: None,
        },
        &mut engine,
        &mut filter,
        &mut job,
        &shared,
    )
    .unwrap_err();
    assert_eq!(error.to_string(), crate::t!("tui_record_current_only"));
    assert_no_cjk(&error.to_string());
    assert!(!engine.status.recording());
    engine.status.station = None;
    assert!(
        worker::execute(
            Command::Record {
                uuid: Some("B".into()),
                duration: None,
                dir: None
            },
            &mut engine,
            &mut filter,
            &mut job,
            &shared
        )
        .is_err()
    );
}

#[test]
fn slow_external_tasks_leave_controls_responsive_and_ignore_stale_results() {
    use crate::test_support::{self, Server, response};
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let mut config = test_support::config(dir.path());
    let fake = test_support::fake_mpv(dir.path());
    config.settings.mpv = fake.to_string_lossy().into();
    let marker = dir.path().join("probe-started");
    let slow = dir.path().join("slow-probe");
    std::fs::write(
        &slow,
        format!("#!/bin/sh\ntouch '{}'\nexec sleep 30\n", marker.display()),
    )
    .unwrap();
    std::fs::set_permissions(&slow, std::fs::Permissions::from_mode(0o700)).unwrap();
    config.settings.ffprobe = slow.to_string_lossy().into();
    let requests = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let seen = requests.clone();
    let server = Server::new(move |_, stream| {
        seen.fetch_add(1, Ordering::Relaxed);
        thread::sleep(Duration::from_secs(2));
        response(stream, 200, "application/json", b"[]");
    });
    config.settings.api_url = Some(server.url.clone());
    let stop = Arc::new(AtomicBool::new(false));
    let engine = Engine::new(config, stop.clone()).unwrap();
    let mut good = station("good");
    good.url = "https://example.com/good".into();
    let mut bad = station("bad");
    bad.url = "https://example.com/fail".into();
    engine.store.upsert(&good).unwrap();
    engine.store.upsert(&bad).unwrap();
    let shared = Arc::new(Mutex::new(View::new(engine.status.clone())));
    let (sender, receiver) = command::channel(16);
    let output = shared.clone();
    let worker_stop = stop.clone();
    let initial_filter = App::restore(&engine.store, engine.config.settings.resume)
        .unwrap()
        .filter;
    let handle =
        thread::spawn(move || worker::run(engine, receiver, output, worker_stop, initial_filter));
    let wait = |check: &dyn Fn(&View) -> bool, budget: Duration| {
        let deadline = Instant::now() + budget;
        loop {
            if check(&shared.lock().unwrap()) {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "worker response exceeded {budget:?}"
            );
            thread::sleep(Duration::from_millis(5));
        }
    };
    sender
        .send(Command::Play(good.stationuuid.clone()))
        .unwrap();
    wait(&|v| v.status.playing(), Duration::from_secs(3));
    // Click reporting is still waiting on a slow HTTP response.
    sender.send(Command::Doctor).unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    while !marker.exists() {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(5));
    }
    sender.send(Command::Volume(37)).unwrap();
    sender.send(Command::Pause).unwrap();
    wait(
        &|v| v.status.volume == 37 && v.status.paused(),
        Duration::from_millis(250),
    );
    sender
        .send(Command::Record {
            uuid: None,
            duration: None,
            dir: None,
        })
        .unwrap();
    wait(&|v| v.status.recording(), Duration::from_secs(1));
    sender.send(Command::RecordStop).unwrap();
    sender.send(Command::Volume(41)).unwrap();
    wait(
        &|v| !v.status.recording() && v.status.volume == 41,
        Duration::from_millis(250),
    );
    sender.send(Command::Play(bad.stationuuid.clone())).unwrap();
    wait(
        &|v| v.status.message == crate::t!("playback_refreshing"),
        Duration::from_secs(2),
    );
    sender.send(Command::Stop).unwrap();
    wait(
        &|v| v.status.message == crate::t!("playback_stopped"),
        Duration::from_millis(250),
    );
    sender
        .send(Command::Play(good.stationuuid.clone()))
        .unwrap();
    wait(
        &|v| {
            v.status.playing() && v.status.station.as_ref().unwrap().stationuuid == good.stationuuid
        },
        Duration::from_secs(2),
    );
    let start = Instant::now();
    stop.store(true, Ordering::Relaxed);
    drop(sender);
    handle.join().unwrap().unwrap();
    assert!(
        start.elapsed() < Duration::from_secs(1),
        "shutdown waited for HTTP or doctor"
    );
    thread::sleep(Duration::from_secs(2));
    assert_eq!(
        shared
            .lock()
            .unwrap()
            .status
            .station
            .as_ref()
            .unwrap()
            .stationuuid,
        good.stationuuid
    );
    assert!(requests.load(Ordering::Relaxed) >= 2);
    let log = std::fs::read_to_string(fake.with_extension("commands")).unwrap();
    assert!(log.contains("[\"add\",\"volume\",-43]"));
    assert!(log.contains("[\"cycle\",\"pause\"]"));
}

#[test]
fn slow_recording_verification_does_not_hold_media_controls_or_shutdown() {
    use crate::test_support::{self, Server, response};
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let mut config = test_support::config(dir.path());
    config.settings.mpv = test_support::fake_mpv(dir.path()).to_string_lossy().into();
    let marker = dir.path().join("verifying");
    let probe = dir.path().join("probe-wrapper");
    std::fs::write(&probe,format!("#!/bin/sh\nfor arg in \"$@\"; do case \"$arg\" in *.part) touch '{}'; exec sleep 30 ;; esac; done\nexec ffprobe \"$@\"\n",marker.display())).unwrap();
    std::fs::set_permissions(&probe, std::fs::Permissions::from_mode(0o700)).unwrap();
    config.settings.ffprobe = probe.to_string_lossy().into();
    let bytes = test_support::fixture("libmp3lame", "mp3");
    let server = Server::new(move |_, s| response(s, 200, "audio/mpeg", &bytes));
    let stop = Arc::new(AtomicBool::new(false));
    let engine = Engine::new(config, stop.clone()).unwrap();
    let mut row = station("record target");
    row.url = server.url.clone();
    row.custom = true;
    engine.store.upsert(&row).unwrap();
    let shared = Arc::new(Mutex::new(View::new(engine.status.clone())));
    let (sender, receiver) = command::channel(16);
    let output = shared.clone();
    let flag = stop.clone();
    let initial_filter = App::restore(&engine.store, engine.config.settings.resume)
        .unwrap()
        .filter;
    let handle = thread::spawn(move || worker::run(engine, receiver, output, flag, initial_filter));
    sender.send(Command::Play(row.stationuuid)).unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    while !shared.lock().unwrap().status.playing() {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(5));
    }
    sender
        .send(Command::Record {
            uuid: None,
            duration: Some(Duration::from_secs(1)),
            dir: None,
        })
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !marker.exists() {
        assert!(
            Instant::now() < deadline,
            "recording never reached verification: {:?}",
            shared.lock().unwrap().logs
        );
        thread::sleep(Duration::from_millis(5));
    }
    let start = Instant::now();
    sender.send(Command::Volume(42)).unwrap();
    sender.send(Command::Pause).unwrap();
    while {
        let data = shared.lock().unwrap();
        data.status.volume != 42 || !data.status.paused()
    } {
        assert!(start.elapsed() < Duration::from_millis(250));
        thread::sleep(Duration::from_millis(5));
    }
    let start = Instant::now();
    stop.store(true, Ordering::Relaxed);
    drop(sender);
    handle.join().unwrap().unwrap();
    assert!(
        start.elapsed() < Duration::from_secs(3),
        "shutdown ignored verification deadline"
    );
    let store = Store::open(&dir.path().join("radio.db")).unwrap();
    let record = store.recordings(1).unwrap().remove(0);
    assert_eq!(record.status, "failed");
    assert!(std::path::Path::new(&record.path).exists());
}

#[test]
fn priority_controls_bypass_a_full_query_queue() {
    let (sender, receiver) = command::channel(1);
    sender.try_send(Command::Search(Filter::default())).unwrap();
    sender.try_send(Command::Stop).unwrap();
    assert!(matches!(receiver.try_recv().unwrap(), Command::Stop));
    assert!(matches!(receiver.try_recv().unwrap(), Command::Search(_)));
}

#[test]
fn recording_refresh_preserves_selected_identity() {
    let recording = |id: &str| {
        serde_json::from_value(serde_json::json!({
        "id":id,"stationuuid":"s","station_name":id,"source_url_resolved":"https://example.com","started_at":"2026-01-01","stopped_at":null,"media_duration_ms":null,"codec":"mp3","container":"mp3","path":"/tmp/test","size_bytes":1,"status":"complete","error":null
    })).unwrap()
    };
    let mut data = view();
    data.recordings = vec![recording("a"), recording("b")].into();
    data.recordings_revision = 1;
    let mut app = App::default();
    app.source = Source::Recordings;
    app.reconcile(&data);
    data.page = Some(super::worker::PageState {
        generation: 2,
        requested: app.filter.clone(),
        phase: super::worker::PagePhase::Loading,
    });
    app.reconcile(&data);
    assert_eq!(app.list.selected(), Some(0));
    data.page.as_mut().unwrap().phase = super::worker::PagePhase::Ready;
    Arc::make_mut(&mut data.recordings).insert(0, recording("new"));
    data.recordings_revision += 1;
    app.reconcile(&data);
    assert_eq!(data.recordings[app.list.selected().unwrap()].id, "a");
}

#[test]
fn refreshing_page_keeps_rows_and_selection_interactive() {
    for source in Source::ALL {
        let (mut app, mut data) = scrollable_list(source, 40);
        let (sender, receiver) = command::channel(4);
        let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
        app.list.select(Some(15));
        *app.list.offset_mut() = 10;
        app.reconcile(&data);
        terminal.draw(|f| render::draw(f, &mut app, &data)).unwrap();
        let rect = app.rects[Focus::List.index()];
        let contents = |terminal: &Terminal<TestBackend>| {
            (rect.y + 1..rect.bottom() - 1)
                .flat_map(|y| {
                    (rect.x + 1..rect.right() - 1)
                        .map(move |x| terminal.backend().buffer()[(x, y)].clone())
                })
                .collect::<Vec<_>>()
        };
        let before = contents(&terminal);
        data.page.as_mut().unwrap().phase = worker::PagePhase::Refreshing;
        let different = Filter {
            query: "different query".into(),
            ..app.filter.clone()
        };
        assert!(!data.page_ready(&different));
        assert!(data.stations_for(&different).is_empty());
        assert!(data.recordings_for(&different).is_empty());
        app.reconcile(&data);
        terminal.draw(|f| render::draw(f, &mut app, &data)).unwrap();
        assert_eq!(contents(&terminal), before, "{source:?}: list flashed");
        assert_eq!((app.list.selected(), app.list.offset()), (Some(15), 10));

        // Input during revalidation remains relative to the visible selection.
        app.key(key(KeyCode::Char('j')), &data, &sender);
        terminal.draw(|f| render::draw(f, &mut app, &data)).unwrap();
        assert_eq!((app.list.selected(), app.list.offset()), (Some(16), 10));
        let selected = if source == Source::Recordings {
            data.recordings[16].id.clone()
        } else {
            data.rows[16].stationuuid.clone()
        };
        // Even if the replacement page reorders rows, keep the latest selection.
        Arc::make_mut(&mut data.rows).reverse();
        Arc::make_mut(&mut data.recordings).reverse();
        data.rows_revision += 1;
        data.recordings_revision += 1;
        data.page.as_mut().unwrap().phase = worker::PagePhase::Ready;
        app.reconcile(&data);
        terminal.draw(|f| render::draw(f, &mut app, &data)).unwrap();
        assert_eq!(app.list.selected(), Some(23), "{source:?}");
        app.key(key(KeyCode::Enter), &data, &sender);
        assert!(match receiver.try_recv().unwrap() {
            Command::Play(id) | Command::PlayRecording(id) => id == selected,
            _ => false,
        });
    }
}

#[test]
fn navigation_during_loading_or_failure_preserves_selection_for_recovery() {
    for source in Source::ALL {
        for phase in [
            worker::PagePhase::Loading,
            worker::PagePhase::Error("storage unavailable".into()),
        ] {
            let (mut app, mut data) = scrollable_list(source, 40);
            let (sender, receiver) = command::channel(4);
            app.list.select(Some(15));
            *app.list.offset_mut() = 10;
            app.reconcile(&data);
            app.inspect_info = true;
            app.details_scroll = 7;
            data.page.as_mut().unwrap().phase = phase;
            for code in ['j', 'k', 'g', 'g', 'G'] {
                app.key(key(KeyCode::Char(code)), &data, &sender);
                app.reconcile(&data);
                assert_eq!((app.list.selected(), app.list.offset()), (Some(15), 10));
                assert!(app.inspect_info);
                assert_eq!(app.details_scroll, 7);
            }
            Arc::make_mut(&mut data.rows).reverse();
            Arc::make_mut(&mut data.recordings).reverse();
            data.rows_revision += 1;
            data.recordings_revision += 1;
            data.page.as_mut().unwrap().phase = worker::PagePhase::Ready;
            app.reconcile(&data);
            assert_eq!(app.list.selected(), Some(24));
            assert!(receiver.try_recv().is_err());
        }
    }
}

#[test]
fn recording_rows_and_details_show_localized_status_duration_and_size() {
    let recording: crate::model::Recording = serde_json::from_value(serde_json::json!({
        "id":"r","stationuuid":"s","station_name":"Night FM","source_url_resolved":"https://example.com",
        "started_at":"2026-01-01","stopped_at":null,"media_duration_ms":65_400,"codec":"mp3",
        "container":"mp3","path":"/tmp/test","size_bytes":1_536_000,"status":"interrupted","error":null
    }))
    .unwrap();
    let mut data = view();
    data.recordings = vec![recording].into();
    data.recordings_revision = 1;
    let mut app = App::default();
    app.source = Source::Recordings;
    app.reconcile(&data);
    let mut terminal = Terminal::new(TestBackend::new(160, 48)).unwrap();
    terminal
        .draw(|frame| render::draw(frame, &mut app, &data))
        .unwrap();
    let screen = text(&terminal);
    assert!(screen.contains(crate::t!("recording_status_interrupted")));
    assert!(screen.contains("01:05"));
    assert!(screen.contains("1.5 MiB"));
    assert!(screen.contains(&crate::t!("size_bytes", "count" => 1_536_000)));
    assert!(!screen.contains("interrupted"));
}

#[test]
fn recordings_list_marks_reconnected_recordings() {
    let recording = |id: &str, reconnects: u64| {
        serde_json::from_value(serde_json::json!({
            "id":id,"stationuuid":"s","station_name":id,"source_url_resolved":"https://example.com",
            "started_at":"2026-01-01","stopped_at":null,"media_duration_ms":null,"codec":"mp3",
            "container":"mp3","path":"/tmp/test","size_bytes":1,"status":"complete","error":null,
            "reconnects":reconnects
        }))
        .unwrap()
    };
    let marker = crate::t!("recording_gap_marker");
    let mut data = view();
    data.recordings = vec![recording("clean", 0), recording("gapped", 2)].into();
    data.recordings_revision = 1;
    let mut app = App::default();
    app.source = Source::Recordings;
    app.reconcile(&data);
    let mut terminal = Terminal::new(TestBackend::new(160, 48)).unwrap();
    terminal
        .draw(|frame| render::draw(frame, &mut app, &data))
        .unwrap();
    // Only the reconnected row carries the marker in the list.
    assert_eq!(text(&terminal).matches(marker).count(), 1);
    data.recordings = vec![recording("clean", 0)].into();
    data.recordings_revision += 1;
    app.reconcile(&data);
    terminal
        .draw(|frame| render::draw(frame, &mut app, &data))
        .unwrap();
    assert!(!text(&terminal).contains(marker));
}

#[test]
fn source_commands_submit_only_the_requested_page() {
    let data = view();
    for (line, source) in [
        ("favorites list --limit 25 --offset 50", Source::Favorites),
        ("history --limit 25 --offset 50", Source::History),
        ("recordings list --limit 25 --offset 50", Source::Recordings),
    ] {
        let (sender, receiver) = command::channel(1);
        let mut app = App::default();
        app.search_due = Some(Instant::now());
        app.mode = Mode::Command;
        app.editor.set(line);
        app.key(key(KeyCode::Enter), &data, &sender);
        let filter = match receiver.try_recv().unwrap() {
            Command::Search(filter) | Command::Recordings(filter) => filter,
            _ => panic!("expected a source query"),
        };
        assert_eq!(app.source, source);
        assert_eq!(filter.limit, 25);
        assert_eq!(filter.offset, 50);
        assert_eq!(filter, app.filter);
        assert!(receiver.try_recv().is_err());
        assert!(
            app.search_due.is_none(),
            "an obsolete query filled the queue"
        );
    }
}

#[test]
fn page_down_uses_totals_and_waits_for_the_current_query() {
    for source in Source::ALL {
        let (sender, receiver) = command::channel(16);
        let mut app = App::default();
        app.source = source;
        let mut data = view();
        data.total = 100;
        data.recordings_total = 100;
        // Even a full last page must stay put.
        Arc::make_mut(&mut data.rows).resize(100, station("row"));
        app.key(key(KeyCode::PageDown), &data, &sender);
        assert_eq!(app.filter.offset, 0);
        assert!(receiver.try_recv().is_err());
        data.total = 200;
        data.recordings_total = 200;
        app.key(key(KeyCode::PageDown), &data, &sender);
        assert_eq!(app.filter.offset, 100);
        receiver.try_recv().unwrap();
        // Repeated keys cannot page using the previous query's total.
        app.key(key(KeyCode::PageDown), &data, &sender);
        assert!(receiver.try_recv().is_err());
        data.filter = app.filter.clone();
        app.key(key(KeyCode::PageDown), &data, &sender);
        assert_eq!(app.filter.offset, 100);
        assert!(receiver.try_recv().is_err());
        app.key(key(KeyCode::PageUp), &data, &sender);
        assert_eq!(app.filter.offset, 0);
        assert!(receiver.try_recv().is_ok());
    }
}

#[test]
fn recordings_keys_play_confirm_delete_open_and_expand_errors() {
    let mut data = view();
    data.recordings = vec![
        serde_json::from_value(serde_json::json!({
            "id":"saved", "stationuuid":"s", "station_name":"Saved audio",
            "source_url_resolved":"https://example.com", "started_at":"2026-01-01",
            "stopped_at":null, "media_duration_ms":null, "codec":"mp3", "container":"mp3",
            "path":"/tmp/sample", "size_bytes":1, "status":"failed",
            "error":"first line\nFFmpeg disk failure details", "reconnects": 2
        }))
        .unwrap(),
    ]
    .into();
    let mut app = App::default();
    app.source = Source::Recordings;
    let (sender, receiver) = command::channel(10);
    app.key(key(KeyCode::Enter), &data, &sender);
    assert!(matches!(receiver.try_recv(), Ok(Command::PlayRecording(id)) if id == "saved"));
    app.key(key(KeyCode::Char('D')), &data, &sender);
    assert!(receiver.try_recv().is_err());
    app.key(key(KeyCode::Char('n')), &data, &sender);
    assert!(receiver.try_recv().is_err());
    app.key(key(KeyCode::Char('D')), &data, &sender);
    app.key(key(KeyCode::Char('y')), &data, &sender);
    assert!(matches!(
        receiver.try_recv(),
        Ok(Command::DeleteRecording {
            index_only: false,
            ..
        })
    ));
    app.key(key(KeyCode::Char('o')), &data, &sender);
    assert!(matches!(receiver.try_recv(), Ok(Command::OpenRecordings)));
    app.key(key(KeyCode::Char('e')), &data, &sender);
    assert!(app.recording_error_expanded);
    let mut terminal = Terminal::new(TestBackend::new(160, 48)).unwrap();
    terminal
        .draw(|frame| render::draw(frame, &mut app, &data))
        .unwrap();
    assert!(text(&terminal).contains("FFmpeg disk failure details"));
    assert!(text(&terminal).contains(&crate::t!("recording_reconnected", "count" => 2)));
}

#[test]
fn ignored_keys_explain_why_nothing_happened() {
    let mut data = view();
    data.rows = Vec::new().into();
    let mut app = App::default();
    let (sender, receiver) = command::channel(10);
    for code in ['y', 'Y', 'f'] {
        app.key(key(KeyCode::Char(code)), &data, &sender);
        assert_eq!(app.message, crate::t!("no_station_selected"));
        assert!(app.clipboard.is_none());
        assert!(receiver.try_recv().is_err());
        app.message.clear();
    }

    app.source = Source::Recordings;
    app.key(key(KeyCode::Char('y')), &data, &sender);
    assert_eq!(app.message, crate::t!("no_recording_selected"));
    for (code, expected) in [
        ('f', crate::t!("recordings_no_favorite")),
        ('F', crate::t!("recordings_no_search")),
        ('/', crate::t!("recordings_no_search")),
    ] {
        app.key(key(KeyCode::Char(code)), &data, &sender);
        assert_eq!(app.message, expected);
        assert_eq!(app.mode, Mode::Normal);
        assert!(app.filter_edit.is_none());
        assert!(receiver.try_recv().is_err());
    }

    // A successful copy still replaces the notice with its own confirmation.
    app.source = Source::Stations;
    let data = view();
    app.key(key(KeyCode::Char('y')), &data, &sender);
    assert_eq!(
        app.message,
        crate::t!("clipboard_station_uuid", "name" => "Selected Station")
    );
    app.source = Source::Stations;
    app.key(key(KeyCode::Char('F')), &data, &sender);
    assert!(app.filter_edit.is_some());
}

#[test]
fn plain_notices_expire_while_dialogs_and_active_tasks_keep_theirs() {
    let ttl = Duration::from_secs(4);
    let ms = Duration::from_millis(1);
    let start = Instant::now();
    let data = view();
    let (sender, receiver) = command::channel(8);
    let mut app = App::default();

    // The lifetime starts when the notice is first seen, and ends exactly once.
    app.message = "Copied".into();
    assert!(!app.expire_message(start));
    assert!(!app.expire_message(start + ttl - ms));
    assert_eq!(app.message, "Copied");
    assert!(app.expire_message(start + ttl));
    assert!(app.message.is_empty());
    assert!(!app.expire_message(start + ttl * 2));

    // A repeated identical notice is a new notice with a fresh lifetime.
    app.message = "Copied".into();
    app.expire_message(start);
    app.key(key(KeyCode::Char('j')), &data, &sender);
    assert!(app.message.is_empty());
    app.message = "Copied".into();
    assert!(!app.expire_message(start + ttl));
    assert!(!app.expire_message(start + ttl * 2 - ms));
    assert!(app.expire_message(start + ttl * 2));

    // Anything owned by a dialog or an unfinished task stays, and its lifetime
    // only starts once it is back on screen.
    let later = start + ttl * 10;
    let held = |app: &mut App, setup: &dyn Fn(&mut App), teardown: &dyn Fn(&mut App)| {
        app.message = "Pending".into();
        setup(app);
        assert!(!app.expire_message(later));
        assert!(!app.expire_message(later + ttl * 10));
        assert_eq!(app.message, "Pending");
        teardown(app);
        assert!(!app.expire_message(later + ttl * 10), "clock restarts");
        assert!(app.expire_message(later + ttl * 11));
        assert!(app.message.is_empty());
    };
    held(&mut app, &|app| app.mode = Mode::Command, &|app| {
        app.mode = Mode::Normal
    });
    held(&mut app, &|app| app.help = true, &|app| app.help = false);
    held(
        &mut app,
        &|app| app.filter_edit = Some((0, Default::default())),
        &|app| app.filter_edit = None,
    );
    held(&mut app, &|app| app.too_small = true, &|app| {
        app.too_small = false
    });

    app.key(key(KeyCode::Char(':')), &data, &sender);
    app.editor
        .set(&format!("station remove {}", data.rows[0].stationuuid));
    app.key(key(KeyCode::Enter), &data, &sender);
    assert!(app.confirmation.is_some());
    app.message = "Confirm removal".into();
    assert!(!app.expire_message(later));
    assert!(!app.expire_message(later + ttl * 10));
    assert_eq!(app.message, "Confirm removal");
    app.key(key(KeyCode::Esc), &data, &sender);
    assert!(app.confirmation.is_none());

    // An unfinished locate keeps its progress notice until it resolves.
    app.jump_binding = "<F6>".into();
    let mut data = view();
    data.status.station = Some(station("Elsewhere"));
    app.key(key(KeyCode::F(6)), &data, &sender);
    assert_eq!(app.message, crate::t!("jump_loading"));
    assert!(!app.expire_message(later));
    assert!(!app.expire_message(later + ttl * 10));
    assert_eq!(app.message, crate::t!("jump_loading"));
    while receiver.try_recv().is_ok() {}
}

#[test]
fn an_expired_notice_gives_the_bottom_bar_back_to_the_context_hint() {
    let data = view();
    let (sender, _receiver) = command::channel(4);
    let mut app = App::default();
    app.key(key(KeyCode::Char('y')), &data, &sender);
    let notice = crate::t!("clipboard_station_uuid", "name" => "Selected Station");
    assert_eq!(app.message, notice);
    let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
    let draw = |app: &mut App, terminal: &mut Terminal<TestBackend>| {
        terminal
            .draw(|frame| render::draw(frame, app, &data))
            .unwrap();
        text(terminal)
    };
    assert!(draw(&mut app, &mut terminal).contains(&notice));
    let start = Instant::now();
    assert!(!app.expire_message(start));
    assert!(app.expire_message(start + Duration::from_secs(4)));
    assert!(!draw(&mut app, &mut terminal).contains(&notice));
}

#[test]
fn help_popup_pages_by_its_visible_height_and_reaches_both_ends() {
    let data = view();
    let (sender, receiver) = command::channel(4);
    let mut app = App::default();
    app.help = true;
    let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
    let draw = |app: &mut App, terminal: &mut Terminal<TestBackend>| {
        terminal
            .draw(|frame| render::draw(frame, app, &data))
            .unwrap();
        text(terminal)
    };
    let ctrl = |c: char| KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL);
    let screen = draw(&mut app, &mut terminal);
    let extent = app.popup;
    assert!(extent.visible > 0 && extent.total > extent.visible * 2);
    let (page, max) = (extent.visible as u16, extent.max() as u16);
    assert!(screen.contains(&format!(" 1-{}/{} ", extent.visible, extent.total)));

    for (event, expected) in [
        (key(KeyCode::PageDown), page),
        (ctrl('f'), page * 2),
        (ctrl('b'), page),
        (key(KeyCode::PageUp), 0),
        (ctrl('b'), 0),
        (ctrl('e'), 1),
        (ctrl('e'), 2),
        (ctrl('y'), 1),
        (key(KeyCode::End), max),
        (key(KeyCode::PageDown), max),
        (key(KeyCode::Char('j')), max),
        (key(KeyCode::Char('g')), 0),
        (key(KeyCode::Char('G')), max),
        (key(KeyCode::Home), 0),
    ] {
        app.key(event, &data, &sender);
        assert_eq!(app.help_scroll, expected, "after {event:?}");
    }
    assert!(app.help);
    assert!(receiver.try_recv().is_err(), "scroll keys never leak");

    app.key(key(KeyCode::End), &data, &sender);
    let screen = draw(&mut app, &mut terminal);
    assert!(screen.contains(&format!(
        " {}-{}/{} ",
        usize::from(max) + 1,
        extent.total,
        extent.total
    )));
    let last = super::help::lines(crate::messages::locale(), &app.jump_binding, 86)
        .pop()
        .unwrap()
        .to_string();
    let last = last.trim_start().to_owned();
    assert!(screen.contains(last.trim_end()), "last help line: {last}");

    // The short title leaves room for the position even at 40 columns; a title
    // that would collide with it wins and the position is dropped.
    let mut narrow = Terminal::new(TestBackend::new(40, 16)).unwrap();
    app.help_scroll = 0;
    let screen = draw(&mut app, &mut narrow);
    assert!(
        screen.contains(&format!("/{} ", app.popup.total)),
        "{screen}"
    );
    app.help_title = Some("A title far too long to share the border");
    let screen = draw(&mut app, &mut narrow);
    assert!(
        !screen.contains(&format!("/{} ", app.popup.total)),
        "{screen}"
    );
}

#[test]
fn long_popup_text_has_no_operation_cap_and_follows_the_terminal_size() {
    let data = view();
    let (sender, _receiver) = command::channel(4);
    let mut app = App::default();
    app.help = true;
    app.help_content = Some(
        (1..=5000)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n"),
    );
    let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
    let draw = |app: &mut App, terminal: &mut Terminal<TestBackend>| {
        terminal
            .draw(|frame| render::draw(frame, app, &data))
            .unwrap();
        text(terminal)
    };
    draw(&mut app, &mut terminal);
    app.key(key(KeyCode::End), &data, &sender);
    let screen = draw(&mut app, &mut terminal);
    assert!(screen.contains("line 5000"), "the tail must be reachable");
    assert!(app.help_scroll > 4096);
    assert_eq!(usize::from(app.help_scroll), app.popup.max());

    // Growing the terminal clamps the offset to the new maximum and pages follow it.
    let mut taller = Terminal::new(TestBackend::new(100, 60)).unwrap();
    draw(&mut app, &mut taller);
    assert_eq!(usize::from(app.help_scroll), app.popup.max());
    assert_eq!(app.popup.visible, 56);
    let before = app.help_scroll;
    app.key(key(KeyCode::PageUp), &data, &sender);
    assert_eq!(app.help_scroll, before - 56);

    // Content that fits again needs no scrolling and shows no position.
    app.help_content = Some("a\nb".into());
    let screen = draw(&mut app, &mut taller);
    assert_eq!(app.help_scroll, 0);
    assert!(app.popup.span(0).is_none());
    assert!(!screen.contains("1-2/2"));
}

#[test]
fn each_popup_names_what_it_shows_and_a_single_failure_gets_its_own_title() {
    use super::worker::{PagePhase, PageState};
    let (sender, _receiver) = command::channel(8);
    let draw = |app: &mut App, data: &View| {
        let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
        terminal.draw(|f| render::draw(f, app, data)).unwrap();
        text(&terminal)
    };
    let failed_playback = |data: &mut View| {
        data.status.playback_state = crate::engine::PlaybackState::Failed {
            reason: "stream refused".into(),
        };
    };
    let failed_recording = |data: &mut View| {
        data.status.recording_state = crate::engine::RecordingState::Failed {
            reason: "disk full".into(),
        };
    };

    let data = view();
    let mut app = App::default();
    app.key(key(KeyCode::Char('?')), &data, &sender);
    assert!(draw(&mut app, &data).contains(crate::t!("help_title")));

    let mut app = App::default();
    for c in ":help record".chars() {
        app.key(key(KeyCode::Char(c)), &data, &sender);
    }
    app.key(key(KeyCode::Enter), &data, &sender);
    let screen = draw(&mut app, &data);
    assert!(screen.contains(crate::t!("command_help_title")), "{screen}");
    assert!(!screen.contains(crate::t!("help_title")));

    let mut playing = view();
    failed_playback(&mut playing);
    let mut app = App::default();
    app.key(key(KeyCode::Char('?')), &playing, &sender);
    let screen = draw(&mut app, &playing);
    assert_eq!(app.help_title, Some(crate::t!("playback_error_title")));
    assert!(
        screen.contains(crate::t!("playback_error_title")),
        "{screen}"
    );
    assert!(screen.contains("stream refused"));

    let mut recording = view();
    failed_recording(&mut recording);
    let mut app = App::default();
    app.key(key(KeyCode::Char('?')), &recording, &sender);
    assert_eq!(app.help_title, Some(crate::t!("recording_error_title")));
    assert!(draw(&mut app, &recording).contains(crate::t!("recording_error_title")));

    let mut operation = view();
    operation.operation_error_id = 1;
    operation.operation_error = "delete failed".into();
    let mut app = App::default();
    app.key(key(KeyCode::Char('?')), &operation, &sender);
    assert_eq!(app.help_title, Some(crate::t!("operation_error_title")));
    assert!(draw(&mut app, &operation).contains(crate::t!("operation_error_title")));

    let mut listing = view();
    let mut app = App::default();
    listing.page = Some(PageState {
        generation: 1,
        requested: app.filter.clone(),
        phase: PagePhase::Error("catalog unreachable".into()),
    });
    app.subtitle_history = true;
    app.key(key(KeyCode::Enter), &listing, &sender);
    assert!(!app.subtitle_history);
    assert_eq!(app.help_title, Some(crate::t!("list_error_title")));
    let screen = draw(&mut app, &listing);
    assert!(screen.contains(crate::t!("list_error_title")), "{screen}");
    assert!(screen.contains("catalog unreachable"));

    // Several failures at once share one generic title and list every detail.
    let mut mixed = view();
    failed_playback(&mut mixed);
    failed_recording(&mut mixed);
    let mut app = App::default();
    app.key(key(KeyCode::Char('?')), &mixed, &sender);
    let screen = draw(&mut app, &mixed);
    assert!(
        screen.contains(crate::t!("error_details_title")),
        "{screen}"
    );
    assert!(screen.contains("stream refused") && screen.contains("disk full"));
    assert_eq!(app.help_title, Some(crate::t!("error_details_title")));
}

#[test]
fn error_and_command_popups_lead_back_to_the_keyboard_help() {
    let (sender, receiver) = command::channel(8);
    let mut data = view();
    data.status.playback_state = crate::engine::PlaybackState::Failed {
        reason: "stream refused\n".to_string() + &"more detail\n".repeat(60),
    };
    let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
    let mut app = App::default();
    app.key(key(KeyCode::Char('?')), &data, &sender);
    terminal.draw(|f| render::draw(f, &mut app, &data)).unwrap();
    assert!(text(&terminal).contains(crate::t!("help_keys_more")));
    app.key(key(KeyCode::PageDown), &data, &sender);
    assert!(app.help_scroll > 0);

    app.key(key(KeyCode::Char('h')), &data, &sender);
    assert!(app.help && app.help_content.is_none() && app.help_title.is_none());
    assert_eq!(app.help_scroll, 0);
    terminal.draw(|f| render::draw(f, &mut app, &data)).unwrap();
    let screen = text(&terminal);
    assert!(screen.contains(crate::t!("help_title")));
    assert!(screen.contains(crate::t!("help_keys")));
    assert!(!screen.contains(crate::t!("help_keys_more")));

    // Inside the keyboard help `h` is plain text, as is Ctrl-h.
    app.key(key(KeyCode::Char('j')), &data, &sender);
    let scrolled = app.help_scroll;
    app.key(key(KeyCode::Char('h')), &data, &sender);
    app.key(
        KeyEvent::new(KeyCode::Char('h'), KeyModifiers::CONTROL),
        &data,
        &sender,
    );
    assert!(app.help && app.help_content.is_none());
    assert_eq!(app.help_scroll, scrolled);

    // Closing and reopening still shows the failure while it lasts.
    app.key(key(KeyCode::Char('q')), &data, &sender);
    assert!(!app.help);
    app.key(key(KeyCode::Char('?')), &data, &sender);
    assert!(
        app.help_content
            .as_deref()
            .unwrap()
            .contains("stream refused")
    );

    let mut app = App::default();
    let idle = view();
    for c in ":help record".chars() {
        app.key(key(KeyCode::Char(c)), &idle, &sender);
    }
    app.key(key(KeyCode::Enter), &idle, &sender);
    assert!(app.help_content.is_some());
    app.key(key(KeyCode::Char('h')), &idle, &sender);
    assert!(app.help_content.is_none());
    assert!(receiver.try_recv().is_err(), "popup keys never leak");
}

#[test]
fn popup_titles_leave_room_for_the_position_on_a_narrow_terminal() {
    let (sender, _receiver) = command::channel(4);
    let mut data = view();
    data.operation_error_id = 1;
    data.operation_error = (1..=40)
        .map(|i| format!("failure line {i}"))
        .collect::<Vec<_>>()
        .join("\n");
    let mut app = App::default();
    app.key(key(KeyCode::Char('?')), &data, &sender);
    let mut terminal = Terminal::new(TestBackend::new(40, 16)).unwrap();
    terminal.draw(|f| render::draw(f, &mut app, &data)).unwrap();
    let screen = text(&terminal);
    assert!(
        screen.contains(crate::t!("operation_error_title")),
        "{screen}"
    );
    assert!(
        screen.contains(&format!("/{} ", app.popup.total)),
        "{screen}"
    );
}

#[test]
fn rendered_text_never_splits_grapheme_clusters() {
    let family = "👨\u{200d}👩\u{200d}👧";
    let fitted = render::fit(&format!("{family} hello"), 5);
    assert_eq!(fitted, format!("{family} h…"));
    assert!(!fitted.ends_with('\u{200d}'));
    assert_eq!(unicode_width::UnicodeWidthStr::width(fitted.as_str()), 5);

    let lines = render::wrapped_lines(&format!("a{family}b{family}c"), 3);
    let rows: Vec<String> = lines.iter().map(ToString::to_string).collect();
    assert_eq!(
        rows,
        [format!("a{family}"), format!("b{family}"), "c".into()]
    );
    assert!(rows.iter().all(|row| !row.starts_with('\u{200d}')));
}

#[test]
fn editor_moves_and_deletes_whole_grapheme_clusters() {
    let ctrl = |c: char| KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL);
    for cluster in [
        "👨\u{200d}👩\u{200d}👧",
        "🇫🇷",
        "e\u{301}",
        "👍\u{1f3fd}",
        "❤\u{fe0f}",
        "你",
    ] {
        let text = format!("a{cluster}b");
        let mut editor = super::state::Editor::default();
        editor.set(&text);
        let offsets = [text.len(), 1 + cluster.len(), 1, 0];
        for &offset in &offsets[1..] {
            editor.key(key(KeyCode::Left), true);
            assert_eq!(editor.cursor, offset, "{cluster:?}");
        }
        editor.key(key(KeyCode::Left), true);
        assert_eq!(editor.cursor, 0);
        for &offset in offsets[..3].iter().rev() {
            editor.key(key(KeyCode::Right), true);
            assert_eq!(editor.cursor, offset, "{cluster:?}");
        }

        editor.set(&text);
        editor.key(key(KeyCode::Left), true);
        editor.key(key(KeyCode::Backspace), true);
        assert_eq!(editor.text, "ab", "{cluster:?}");
        assert_eq!(editor.cursor, 1);

        editor.set(&text);
        editor.cursor = 1;
        editor.key(key(KeyCode::Delete), true);
        assert_eq!(editor.text, "ab", "{cluster:?}");
        assert_eq!(editor.cursor, 1);

        editor.set(&format!("play {cluster}"));
        editor.key(ctrl('w'), true);
        assert_eq!(editor.text, "play ", "{cluster:?}");

        // Typing at the cursor keeps working after a cluster edit.
        editor.key(key(KeyCode::Char('x')), true);
        assert_eq!(editor.text, "play x");
    }

    // A cursor left inside a cluster must not panic or leave half a cluster.
    let mut editor = super::state::Editor::default();
    let family = "👨\u{200d}👩\u{200d}👧";
    editor.set(&format!("a{family}b"));
    editor.cursor = 1 + '👨'.len_utf8();
    editor.key(key(KeyCode::Delete), true);
    assert_eq!(editor.text, "a👨b");
    editor.set(&format!("a{family}b"));
    editor.cursor = 1 + '👨'.len_utf8();
    editor.key(key(KeyCode::Left), true);
    assert_eq!(editor.cursor, 1);
}

#[test]
fn pasted_text_edits_the_open_input_without_submitting_or_acting() {
    let mut editor = super::state::Editor::default();
    for (pasted, expected) in [
        ("play abc", "play abc"),
        ("a\nb\r\nc\rd\te", "a b c d e"),
        ("x\u{1b}[31my\u{7f}z\u{0}", "x[31myz"),
        ("粘贴 👨\u{200d}👩\u{200d}👧", "粘贴 👨\u{200d}👩\u{200d}👧"),
        ("\n", " "),
        ("", ""),
    ] {
        editor.begin("");
        editor.paste(pasted);
        assert_eq!(editor.text, expected, "{pasted:?}");
        assert_eq!(editor.cursor, expected.len());
    }

    editor.begin("ab");
    editor.cursor = 1;
    editor.paste("XY");
    assert_eq!((editor.text.as_str(), editor.cursor), ("aXYb", 3));

    // The shared line limit applies, and a cluster is dropped rather than cut.
    let family = "👨\u{200d}👩\u{200d}👧";
    editor.begin(&"a".repeat(2030));
    editor.paste(&format!("{family}{family}"));
    assert_eq!(editor.text.len(), 2030 + family.len());
    assert!(editor.text.ends_with(family));
    editor.begin(&"a".repeat(2040));
    editor.paste(family);
    assert_eq!(editor.text.len(), 2040);
    editor.begin("");
    editor.paste(&"b".repeat(100_000));
    assert_eq!(editor.text.len(), 2048);
}

#[test]
fn paste_reaches_command_search_and_filter_fields_but_never_shortcuts() {
    let data = view();
    let (sender, receiver) = command::channel(10);

    let mut app = App::default();
    app.key(key(KeyCode::Char(':')), &data, &sender);
    app.paste("station add pasted\nhttps://example.com/p\n");
    assert_eq!(app.mode, Mode::Command);
    assert_eq!(app.editor.text, "station add pasted https://example.com/p ");
    assert!(receiver.try_recv().is_err());

    let mut app = App::default();
    app.key(key(KeyCode::Char('/')), &data, &sender);
    app.paste("jazz\nfm");
    assert_eq!(app.mode, Mode::Search);
    assert_eq!(app.filter.query, "jazz fm");
    assert!(app.search_due.is_some());

    let mut app = App::default();
    app.key(key(KeyCode::Char('F')), &data, &sender);
    app.key(key(KeyCode::Tab), &data, &sender);
    app.paste("fr\nench");
    assert_eq!(app.filter_edit.as_ref().unwrap().1[1], "fr ench");
    assert_eq!(app.filter_input.text, "fr ench");

    // Normal mode, help, confirmation and tiny terminals ignore the payload.
    let mut app = App::default();
    app.paste("q:r m\nn");
    assert!(!app.quit);
    assert_eq!(app.message, crate::t!("paste_ignored"));
    assert!(receiver.try_recv().is_err());
    app.message.clear();
    app.help = true;
    app.paste("q?x");
    assert!(app.help && app.message.is_empty());
    app.help = false;
    app.update_size(20, 10);
    app.paste("q");
    assert!(!app.quit && app.message.is_empty());
    app.update_size(100, 24);
    app.key(key(KeyCode::Char(':')), &data, &sender);
    app.editor.set("");
    app.update_size(20, 10);
    app.paste("hidden");
    assert!(app.editor.text.is_empty());
}

#[test]
fn auto_repeat_moves_and_edits_but_never_triggers_actions() {
    use crossterm::event::KeyEventKind::Repeat;
    let repeat = |code| KeyEvent::new_with_kind(code, KeyModifiers::NONE, Repeat);
    let data = view();
    let (sender, receiver) = command::channel(20);
    let mut app = App::default();
    let focus = app.focus;
    for code in [
        'r', 'n', 'm', ' ', 'f', 'y', 'Y', 'F', ':', '/', '?', 'q', 'o', 'e', 'D', '[', ']', 'g',
    ] {
        app.key(repeat(KeyCode::Char(code)), &data, &sender);
    }
    for code in [KeyCode::Enter, KeyCode::Tab, KeyCode::BackTab, KeyCode::Esc] {
        app.key(repeat(code), &data, &sender);
    }
    assert!(receiver.try_recv().is_err());
    assert!(!app.quit && !app.help && app.filter_edit.is_none());
    assert!(app.clipboard.is_none() && app.message.is_empty());
    assert_eq!(
        (app.mode, app.focus, app.source),
        (Mode::Normal, focus, Source::Stations)
    );

    // Navigation and volume are meant to repeat.
    app.key(repeat(KeyCode::Char('j')), &data, &sender);
    assert_eq!(app.list.selected(), Some(1));
    app.key(repeat(KeyCode::Char('+')), &data, &sender);
    assert!(matches!(receiver.try_recv(), Ok(Command::AdjustVolume(5))));
    // A fresh press of the same action key still works.
    app.key(key(KeyCode::Char('m')), &data, &sender);
    assert!(matches!(receiver.try_recv(), Ok(Command::Mute)));

    // Text editing repeats, but a held Enter must not submit the line.
    app.key(key(KeyCode::Char(':')), &data, &sender);
    app.key(repeat(KeyCode::Char('a')), &data, &sender);
    app.key(repeat(KeyCode::Char('b')), &data, &sender);
    app.key(repeat(KeyCode::Backspace), &data, &sender);
    app.key(repeat(KeyCode::Enter), &data, &sender);
    assert_eq!((app.mode, app.editor.text.as_str()), (Mode::Command, "a"));
    assert!(receiver.try_recv().is_err());

    // Help scrolls on repeat but does not close.
    let mut app = App::default();
    app.key(key(KeyCode::Char('?')), &data, &sender);
    app.key(repeat(KeyCode::Char('j')), &data, &sender);
    app.key(repeat(KeyCode::Esc), &data, &sender);
    assert!(app.help);

    // A held key cannot confirm a deletion or its cancel path.
    let mut app = App::default();
    app.source = Source::Recordings;
    let mut recorded = view();
    recorded.recordings = vec![
        serde_json::from_value(serde_json::json!({
            "id":"saved", "stationuuid":"s", "station_name":"Saved audio",
            "source_url_resolved":"https://example.com", "started_at":"2026-01-01",
            "stopped_at":"2026-01-01", "media_duration_ms":null, "codec":"mp3",
            "container":"mp3", "path":"/tmp/sample", "size_bytes":1, "status":"complete",
            "error":null, "reconnects": 0
        }))
        .unwrap(),
    ]
    .into();
    app.key(key(KeyCode::Char('D')), &recorded, &sender);
    assert!(app.confirmation.is_some());
    app.key(repeat(KeyCode::Char('y')), &recorded, &sender);
    app.key(repeat(KeyCode::Esc), &recorded, &sender);
    assert!(app.confirmation.is_some());
    assert!(receiver.try_recv().is_err());
}

#[test]
fn ctrl_line_scroll_moves_the_viewport_while_preserving_a_visible_selection() {
    let ctrl = |c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL);
    for source in Source::ALL {
        let (mut app, data) = scrollable_list(source, 40);
        let (sender, receiver) = command::channel(4);
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|f| render::draw(f, &mut app, &data)).unwrap();
        app.list.select(Some(15));
        *app.list.offset_mut() = 10;
        app.inspect_info = true;
        app.details_scroll = 7;
        terminal.draw(|f| render::draw(f, &mut app, &data)).unwrap();
        let rect = app.rects[Focus::List.index()];
        let top_row = |terminal: &Terminal<TestBackend>| {
            (rect.x + 1..rect.right() - 1)
                .map(|x| terminal.backend().buffer()[(x, rect.y + 1)].symbol())
                .collect::<String>()
        };
        assert!(top_row(&terminal).contains("Row 010"), "{source:?}");
        let selected_id = if source == Source::Recordings {
            data.recordings[15].id.clone()
        } else {
            data.rows[15].stationuuid.clone()
        };
        for (code, offset, repeat) in [
            ('e', 11, false),
            ('y', 10, false),
            ('y', 9, false),
            ('e', 10, true),
        ] {
            let mut event = ctrl(code);
            if repeat {
                event.kind = crossterm::event::KeyEventKind::Repeat;
            }
            app.key(event, &data, &sender);
            terminal.draw(|f| render::draw(f, &mut app, &data)).unwrap();
            assert_eq!(app.list.offset(), offset, "{source:?}, Ctrl-{code}");
            assert_eq!(app.list.selected(), Some(15), "{source:?}, Ctrl-{code}");
            assert!(top_row(&terminal).contains(&format!("Row {offset:03}")));
            assert!(app.inspect_info);
            assert_eq!(app.details_scroll, 7);
        }
        // Enter still targets the same row; scrolling itself submits no work.
        assert!(receiver.try_recv().is_err());
        app.key(key(KeyCode::Enter), &data, &sender);
        assert!(match receiver.try_recv().unwrap() {
            Command::Play(uuid) => uuid == selected_id,
            Command::PlayRecording(id) => id == selected_id,
            _ => panic!("expected playback of the selected row"),
        });
    }
}

#[test]
fn ctrl_line_scroll_moves_edge_selections_just_enough_to_keep_them_visible() {
    for source in Source::ALL {
        for code in ['e', 'y'] {
            let (mut app, mut data) = scrollable_list(source, 40);
            let (sender, receiver) = command::channel(4);
            let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
            terminal.draw(|f| render::draw(f, &mut app, &data)).unwrap();
            let height = usize::from(app.rects[Focus::List.index()].height - 2);
            let selected = if code == 'e' { 10 } else { 10 + height - 1 };
            let expected = if code == 'e' {
                selected + 1
            } else {
                selected - 1
            };
            app.list.select(Some(selected));
            *app.list.offset_mut() = 10;
            app.reconcile(&data);
            app.inspect_info = true;
            app.details_scroll = 7;
            app.key(
                KeyEvent::new(KeyCode::Char(code), KeyModifiers::CONTROL),
                &data,
                &sender,
            );
            terminal.draw(|f| render::draw(f, &mut app, &data)).unwrap();
            assert_eq!(app.list.offset(), if code == 'e' { 11 } else { 9 });
            assert_eq!(app.list.selected(), Some(expected));
            assert!(!app.inspect_info);
            assert_eq!(app.details_scroll, 0);
            // A later refresh preserves the newly selected identity.
            Arc::make_mut(&mut data.rows).reverse();
            Arc::make_mut(&mut data.recordings).reverse();
            data.rows_revision += 1;
            data.recordings_revision += 1;
            app.reconcile(&data);
            assert_eq!(app.list.selected(), Some(39 - expected));
            assert!(receiver.try_recv().is_err());
        }
    }
}

#[test]
fn ctrl_line_scroll_handles_short_pages_bounds_resize_and_unavailable_rows() {
    let ctrl = |c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL);
    let (sender, receiver) = command::channel(4);
    let (mut app, mut data) = scrollable_list(Source::Stations, 3);
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    terminal.draw(|f| render::draw(f, &mut app, &data)).unwrap();
    app.list.select(Some(2));
    for (code, offset) in [
        ('y', 0),
        ('e', 1),
        ('e', 2),
        ('e', 2),
        ('y', 1),
        ('y', 0),
        ('y', 0),
    ] {
        app.key(ctrl(code), &data, &sender);
        terminal.draw(|f| render::draw(f, &mut app, &data)).unwrap();
        assert_eq!(app.list.offset(), offset);
        assert_eq!(app.list.selected(), Some(2));
    }
    // A smaller viewport clamps the selection only when scrolling hides it.
    let (mut resized, resized_data) = scrollable_list(Source::Stations, 40);
    terminal
        .draw(|f| render::draw(f, &mut resized, &resized_data))
        .unwrap();
    resized.list.select(Some(15));
    *resized.list.offset_mut() = 10;
    terminal.backend_mut().resize(40, 16);
    terminal
        .draw(|f| render::draw(f, &mut resized, &resized_data))
        .unwrap();
    let previous = resized.list.offset();
    resized.key(ctrl('y'), &resized_data, &sender);
    terminal
        .draw(|f| render::draw(f, &mut resized, &resized_data))
        .unwrap();
    assert_eq!(resized.list.offset(), previous - 1);
    assert_eq!(resized.list.selected(), Some(14));

    for phase in [
        worker::PagePhase::Loading,
        worker::PagePhase::Error("query failed".into()),
    ] {
        data.page.as_mut().unwrap().phase = phase;
        app.key(ctrl('e'), &data, &sender);
        assert_eq!((app.list.offset(), app.list.selected()), (0, Some(2)));
    }
    data.page.as_mut().unwrap().phase = worker::PagePhase::Ready;
    app.rects[Focus::List.index()].height = 2;
    app.key(ctrl('e'), &data, &sender);
    assert_eq!((app.list.offset(), app.list.selected()), (0, Some(2)));
    let (mut empty, empty_data) = scrollable_list(Source::Stations, 0);
    terminal
        .draw(|f| render::draw(f, &mut empty, &empty_data))
        .unwrap();
    empty.key(ctrl('e'), &empty_data, &sender);
    empty.key(ctrl('y'), &empty_data, &sender);
    assert_eq!(empty.list.offset(), 0);
    assert!(receiver.try_recv().is_err());
}

fn scrollable_list(source: Source, len: usize) -> (App, View) {
    let mut app = App::default();
    app.source = source;
    app.filter.favorites = source == Source::Favorites;
    app.filter.history = source == Source::History;
    app.filter.recordings = source == Source::Recordings;
    let mut data = view();
    data.rows = Arc::new((0..len).map(|n| station(&format!("Row {n:03}"))).collect());
    data.recordings = Arc::new((0..len).map(|n| {
        serde_json::from_value(serde_json::json!({
            "id":format!("recording-{n}"), "stationuuid":"station", "station_name":format!("Row {n:03}"),
            "source_url_resolved":"https://example.com", "started_at":"2026-01-01", "stopped_at":null,
            "media_duration_ms":null, "codec":"mp3", "container":"mp3", "path":"/tmp/clip.mp3",
            "size_bytes":1, "status":"complete", "error":null
        })).unwrap()
    }).collect());
    data.filter = app.filter.clone();
    data.page.as_mut().unwrap().requested = app.filter.clone();
    data.total = len;
    data.recordings_total = len;
    (app, data)
}

#[test]
fn ctrl_aliases_scroll_and_page_by_focus_without_leaking_into_inputs() {
    let ctrl = |c: char| KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL);
    let mut data = view();
    Arc::make_mut(&mut data.rows).resize(30, station("row"));
    data.total = 500;
    let (tx, rx) = command::channel(16);

    // List: Ctrl-e/y scroll the viewport; Ctrl-f/b turn catalog pages.
    let mut app = App::default();
    app.focus = Focus::List;
    app.list.select(Some(5));
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    terminal.draw(|f| render::draw(f, &mut app, &data)).unwrap();
    app.key(ctrl('e'), &data, &tx);
    assert_eq!(app.list.selected(), Some(5));
    assert_eq!(app.list.offset(), 1);
    app.key(ctrl('y'), &data, &tx);
    app.key(ctrl('y'), &data, &tx);
    assert_eq!(app.list.selected(), Some(5));
    assert_eq!(app.list.offset(), 0);
    assert!(rx.try_recv().is_err());
    app.key(ctrl('f'), &data, &tx);
    assert_eq!(app.filter.offset, app.filter.limit);
    assert!(rx.try_recv().is_ok());
    data.filter = app.filter.clone();
    app.key(ctrl('b'), &data, &tx);
    assert_eq!(app.filter.offset, 0);
    assert!(rx.try_recv().is_ok());

    // Details and Activity scroll; Sources and Player have nothing to scroll.
    app.focus = Focus::Details;
    for (code, expected) in [('e', 1), ('f', 11), ('y', 10), ('b', 0), ('b', 0)] {
        app.key(ctrl(code), &data, &tx);
        assert_eq!(app.details_scroll, expected, "Ctrl-{code}");
    }
    app.focus = Focus::Activity;
    app.activity_scroll = 30;
    for (code, expected) in [('e', 29), ('f', 19), ('y', 20), ('b', 30)] {
        app.key(ctrl(code), &data, &tx);
        assert_eq!(app.activity_scroll, expected, "Ctrl-{code}");
    }
    for focus in [Focus::Sources, Focus::Player] {
        app.focus = focus;
        for code in ['e', 'y', 'f', 'b'] {
            app.key(ctrl(code), &data, &tx);
        }
        assert_eq!((app.focus, app.source), (focus, Source::Stations));
    }
    assert!(rx.try_recv().is_err());

    // A held key still scrolls when the terminal reports repeats.
    app.focus = Focus::Details;
    app.details_scroll = 0;
    app.key(
        KeyEvent::new_with_kind(
            KeyCode::Char('e'),
            KeyModifiers::CONTROL,
            crossterm::event::KeyEventKind::Repeat,
        ),
        &data,
        &tx,
    );
    assert_eq!(app.details_scroll, 1);

    // A configured locate binding keeps precedence over the aliases.
    let mut located = view();
    Arc::make_mut(&mut located.rows).resize(30, station("row"));
    Arc::make_mut(&mut located.rows).push(located.status.station.clone().unwrap());
    let mut app = App::default();
    app.jump_binding = "<Ctrl-e>".into();
    app.list.select(Some(0));
    app.key(ctrl('e'), &located, &tx);
    assert_eq!(app.list.selected(), Some(30));

    // Text inputs and the help overlay never run the aliases.
    for open in ['/', ':', '?', 'F'] {
        let mut app = App::default();
        app.list.select(Some(5));
        app.key(key(KeyCode::Char(open)), &data, &tx);
        let _ = rx.try_recv();
        for code in ['e', 'y', 'f', 'b'] {
            app.key(ctrl(code), &data, &tx);
        }
        assert_eq!(app.list.selected(), Some(5), "after {open}");
        assert_eq!((app.help_scroll, app.filter.offset), (0, 0), "after {open}");
        assert!(app.editor.text.is_empty(), "after {open}");
        assert!(rx.try_recv().is_err(), "after {open}");
    }
}

#[test]
fn search_cursor_keys_keep_the_selection_and_do_not_requery() {
    let data = view();
    let (tx, rx) = command::channel(8);
    let mut app = App::default();
    app.key(key(KeyCode::Char('/')), &data, &tx);
    for c in "ab".chars() {
        app.key(key(KeyCode::Char(c)), &data, &tx);
    }
    assert_eq!(app.filter.query, "ab");
    assert!(app.search_due.is_some());

    app.search_due = None;
    app.list.select(Some(1));
    app.filter.offset = 50;
    let ctrl = |c: char| KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL);
    for event in [
        key(KeyCode::Left),
        key(KeyCode::Right),
        key(KeyCode::Home),
        key(KeyCode::End),
        key(KeyCode::Delete),
        ctrl('a'),
        ctrl('e'),
        ctrl('y'),
    ] {
        app.key(event, &data, &tx);
    }
    assert_eq!(app.mode, Mode::Search);
    assert_eq!((app.filter.query.as_str(), app.filter.offset), ("ab", 50));
    assert_eq!(app.list.selected(), Some(1));
    assert!(app.search_due.is_none());
    assert!(rx.try_recv().is_err());

    // Editing the text still restarts the query from the first page.
    app.key(key(KeyCode::Backspace), &data, &tx);
    assert_eq!((app.filter.query.as_str(), app.filter.offset), ("a", 0));
    assert_eq!(app.list.selected(), Some(0));
    assert!(app.search_due.is_some());
}

fn paged_view(app: &App, rows: Vec<Station>) -> View {
    let mut data = view();
    data.total = 300;
    data.rows = rows.into();
    data.filter = app.filter.clone();
    data.page = Some(worker::PageState {
        generation: 0,
        requested: data.filter.clone(),
        phase: worker::PagePhase::Ready,
    });
    data.rows_revision += 1;
    data
}

#[test]
fn escaping_the_search_restores_the_page_and_selected_row() {
    let (tx, rx) = command::channel(8);
    let mut app = App::default();
    app.filter.offset = 100;
    let first = station("First");
    let second = station("Second");
    let data = paged_view(&app, vec![first.clone(), second.clone()]);
    app.reconcile(&data);
    app.key(key(KeyCode::Char('j')), &data, &tx);
    assert_eq!(app.selected(&data).unwrap().name, "Second");

    app.key(key(KeyCode::Char('/')), &data, &tx);
    for c in "xy".chars() {
        app.key(key(KeyCode::Char(c)), &data, &tx);
    }
    assert_eq!((app.filter.query.as_str(), app.filter.offset), ("xy", 0));
    assert_eq!(app.list.selected(), Some(0));
    // Deleting the text again leaves the query as it was, but the page and
    // the row were already reset by the edits.
    for _ in 0..2 {
        app.key(key(KeyCode::Backspace), &data, &tx);
    }
    assert_eq!(app.filter.offset, 0);

    app.key(key(KeyCode::Esc), &data, &tx);
    assert_eq!(app.mode, Mode::Normal);
    assert_eq!((app.filter.query.as_str(), app.filter.offset), ("", 100));
    assert_eq!(app.list.selected(), Some(1));
    app.flush_search(&tx);
    let searches: Vec<_> = rx
        .try_iter()
        .filter_map(|c| match c {
            Command::Search(filter) => Some((filter.query, filter.offset)),
            _ => None,
        })
        .collect();
    assert_eq!(searches, vec![(String::new(), 100)]);

    // The page that comes back may have shifted; the row is found by its ID.
    let mut shifted = paged_view(&app, vec![station("New"), first, second]);
    shifted.rows_revision += 1;
    app.reconcile(&shifted);
    assert_eq!(app.selected(&shifted).unwrap().name, "Second");
}

#[test]
fn escaping_an_untouched_search_changes_nothing() {
    let (tx, rx) = command::channel(8);
    let mut app = App::default();
    app.filter.offset = 100;
    app.filter.query = "radio".into();
    let data = paged_view(&app, vec![station("First"), station("Second")]);
    app.reconcile(&data);
    app.key(key(KeyCode::Char('j')), &data, &tx);

    app.key(key(KeyCode::Char('/')), &data, &tx);
    for event in [key(KeyCode::Left), key(KeyCode::End), key(KeyCode::Esc)] {
        app.key(event, &data, &tx);
    }
    assert_eq!(app.mode, Mode::Normal);
    assert_eq!(
        (
            app.filter.query.as_str(),
            app.filter.offset,
            app.list.selected()
        ),
        ("radio", 100, Some(1))
    );
    assert!(app.search_due.is_none());
    assert!(rx.try_recv().is_err());
}

#[test]
fn confirming_a_search_keeps_the_new_query_and_starts_at_the_first_page() {
    let (tx, rx) = command::channel(8);
    let mut app = App::default();
    app.filter.offset = 100;
    let data = paged_view(&app, vec![station("First"), station("Second")]);
    app.reconcile(&data);

    app.key(key(KeyCode::Char('/')), &data, &tx);
    app.key(key(KeyCode::Char('z')), &data, &tx);
    app.key(key(KeyCode::Enter), &data, &tx);
    assert_eq!((app.filter.query.as_str(), app.filter.offset), ("z", 0));
    assert!(matches!(rx.try_recv(), Ok(Command::Search(f)) if f.query == "z" && f.offset == 0));

    // A later, untouched search does not remember the old origin.
    app.key(key(KeyCode::Char('/')), &data, &tx);
    app.key(key(KeyCode::Esc), &data, &tx);
    assert_eq!((app.filter.query.as_str(), app.filter.offset), ("z", 0));
    assert!(rx.try_recv().is_err());
}

fn searched_offsets(rx: &command::CommandReceiver) -> Vec<usize> {
    rx.try_iter()
        .filter_map(|c| match c {
            Command::Search(filter) => Some(filter.offset),
            _ => None,
        })
        .collect()
}

fn assert_sources_come_back_where_they_were(mut app: App) {
    let (tx, rx) = command::channel(16);
    let home = app.source;
    let other = Source::ALL[(home.index() + 1) % 4];
    app.filter.offset = 200;
    let rows = vec![station("A"), station("B"), station("C")];
    let data = paged_view(&app, rows.clone());
    app.reconcile(&data);
    app.key(key(KeyCode::Char('j')), &data, &tx);
    app.key(key(KeyCode::Char('j')), &data, &tx);
    assert_eq!(app.selected(&data).unwrap().name, "C");

    app.key(key(KeyCode::Char(']')), &data, &tx);
    assert_eq!((app.source, app.filter.offset), (other, 0));
    let (x, y) = (station("X"), station("Y"));
    let theirs = paged_view(&app, vec![x.clone(), y.clone()]);
    app.reconcile(&theirs);
    app.key(key(KeyCode::Char('j')), &theirs, &tx);
    assert_eq!(app.selected(&theirs).unwrap().name, "Y");

    app.key(key(KeyCode::Char('[')), &theirs, &tx);
    assert_eq!((app.source, app.filter.offset), (home, 200));
    assert_eq!(searched_offsets(&rx), vec![0, 200]);
    // Every page here carries the same revision number: the row is still
    // found by its ID, and the page may have shifted meanwhile.
    let back = paged_view(
        &app,
        vec![
            station("New"),
            rows[0].clone(),
            rows[1].clone(),
            rows[2].clone(),
        ],
    );
    app.reconcile(&back);
    assert_eq!(app.selected(&back).unwrap().name, "C");

    app.key(key(KeyCode::Char(']')), &back, &tx);
    assert_eq!((app.source, app.filter.offset), (other, 0));
    let again = paged_view(&app, vec![station("W"), x, y]);
    app.reconcile(&again);
    assert_eq!(app.selected(&again).unwrap().name, "Y");
}

#[test]
fn sources_come_back_to_their_page_and_row_in_either_resume_state() {
    use crate::config::ResumeMode;
    assert_sources_come_back_where_they_were(App::default());

    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("radio.db")).unwrap();
    let saved = station("Saved");
    store.upsert(&saved).unwrap();
    store.favorite(&saved.stationuuid, true).unwrap();
    store.set_meta("pref_source", "favorites").unwrap();
    store
        .set_meta("pref_last_station", &saved.stationuuid)
        .unwrap();
    for mode in [ResumeMode::Disabled, ResumeMode::Selection] {
        let app = App::restore(&store, mode).unwrap();
        assert_eq!(
            app.source,
            if mode == ResumeMode::Disabled {
                Source::Stations
            } else {
                Source::Favorites
            }
        );
        assert_sources_come_back_where_they_were(app);
    }
}

#[test]
fn a_vanished_row_or_a_changed_query_falls_back_to_the_first_row_or_page() {
    let (tx, rx) = command::channel(16);
    let mut app = App::default();
    app.filter.offset = 200;
    let rows = vec![station("A"), station("B"), station("C")];
    let data = paged_view(&app, rows.clone());
    app.reconcile(&data);
    app.key(key(KeyCode::Char('j')), &data, &tx);
    app.key(key(KeyCode::Char(']')), &data, &tx);
    app.reconcile(&paged_view(&app, vec![station("X")]));
    app.key(key(KeyCode::Char('[')), &data, &tx);
    assert_eq!(app.filter.offset, 200);
    // "B" was removed while the user was elsewhere.
    let back = paged_view(&app, vec![rows[0].clone(), rows[2].clone()]);
    app.reconcile(&back);
    assert_eq!(app.list.selected(), Some(0));

    // Another query in the other source makes the old page meaningless.
    app.key(key(KeyCode::Char(']')), &back, &tx);
    app.reconcile(&paged_view(&app, vec![station("X")]));
    app.key(key(KeyCode::Char('/')), &back, &tx);
    app.key(key(KeyCode::Char('q')), &back, &tx);
    app.key(key(KeyCode::Enter), &back, &tx);
    app.key(key(KeyCode::Char('[')), &back, &tx);
    assert_eq!((app.filter.query.as_str(), app.filter.offset), ("q", 0));
    assert_eq!(searched_offsets(&rx).last(), Some(&0));
}

#[test]
fn an_explicit_page_beats_the_remembered_one() {
    let (tx, _rx) = command::channel(16);
    let mut app = App::default();
    let data = paged_view(&app, vec![station("A"), station("B")]);
    app.reconcile(&data);
    app.key(key(KeyCode::Char(']')), &data, &tx);
    app.filter.offset = 100;
    app.key(key(KeyCode::Char('[')), &data, &tx);
    assert_eq!(app.source, Source::Stations);

    app.mode = Mode::Command;
    app.editor.set("favorites list --limit 100 --offset 50");
    app.key(key(KeyCode::Enter), &data, &tx);
    assert_eq!(app.source, Source::Favorites);
    assert_eq!((app.filter.offset, app.filter.limit), (50, 100));
    assert_eq!(app.list.selected(), Some(0));
}

#[test]
fn media_states_are_distinguishable_in_header_list_row_and_details() {
    use crate::engine::PlaybackState as P;
    let reason = "HTTP 503 from the upstream station while opening the stream";
    for (state, glyph, label, color) in [
        (P::Connecting, "…", "Connecting", Color::Yellow),
        (
            P::Reconnecting { attempt: 3 },
            "↻",
            "Reconnecting (attempt 3)",
            Color::Yellow,
        ),
        (P::Playing, "▶", "Playing", Color::Green),
        (P::Paused, "‖", "Paused", Color::Yellow),
        (
            P::Failed {
                reason: reason.into(),
            },
            "!",
            "Playback failed: HTTP 503 from",
            Color::Red,
        ),
    ] {
        let mut data = view();
        let current = data.status.station.clone().unwrap();
        Arc::make_mut(&mut data.rows)[0] = current.clone();
        Arc::make_mut(&mut data.rows)[0].favorite = true;
        data.status.playback_state = state.clone();
        let mut app = App::default();
        app.list.select(Some(1));
        let mut terminal = Terminal::new(TestBackend::new(120, 24)).unwrap();
        terminal
            .draw(|frame| render::draw(frame, &mut app, &data))
            .unwrap();
        let rendered = text(&terminal);
        let lines: Vec<&str> = rendered.lines().collect();
        assert!(
            lines
                .iter()
                .any(|line| line.contains(&format!("│{glyph} {label}"))),
            "{state:?} header\n{rendered}"
        );
        // The marker sits in its own slot before the favourite star.
        assert!(
            lines
                .iter()
                .any(|line| line.contains(&format!("  {glyph}★ Current Station"))),
            "{state:?} row\n{rendered}"
        );
        assert!(
            lines
                .iter()
                .all(|line| !line.contains(&format!("{glyph}  Another Station"))),
            "{state:?} marks another station\n{rendered}"
        );
        let rect = app.rects[Focus::List.index()];
        let buffer = terminal.backend().buffer();
        let marker = (rect.y..rect.bottom())
            .flat_map(|y| (rect.x..rect.right()).map(move |x| (x, y)))
            .find(|&(x, y)| buffer[(x, y)].symbol() == glyph && buffer[(x + 1, y)].symbol() == "★")
            .unwrap_or_else(|| panic!("{state:?} list marker missing\n{rendered}"));
        assert_eq!(buffer[marker].fg, color, "{state:?}");
        let details = render::station_details(&App::default(), &current, &data);
        let first = details.lines().next().unwrap();
        assert!(
            first.contains(&format!(" · {glyph} {}", state.label())),
            "{state:?}\n{details}"
        );
    }

    // Idle shows no marker and describes the station as merely selected.
    let mut data = view();
    let current = data.status.station.clone().unwrap();
    Arc::make_mut(&mut data.rows)[0] = current.clone();
    data.status.playback_state = P::Idle;
    let mut app = App::default();
    app.list.select(Some(1));
    let mut terminal = Terminal::new(TestBackend::new(120, 24)).unwrap();
    terminal
        .draw(|frame| render::draw(frame, &mut app, &data))
        .unwrap();
    let rendered = text(&terminal);
    assert!(rendered.contains("│Idle · Current Station"), "{rendered}");
    assert!(rendered.contains("┃     Current Station"), "{rendered}");
    assert!(
        render::station_details(&App::default(), &current, &data)
            .contains(crate::t!("details_origin_selected"))
    );
}

#[test]
fn failure_reason_in_the_header_scales_with_the_terminal_width() {
    let reason = "HTTP 503 from the upstream station while opening the stream";
    let mut data = view();
    data.status.playback_state = crate::engine::PlaybackState::Failed {
        reason: reason.into(),
    };
    let mut shown = vec![];
    for width in [80u16, 120, 200] {
        let mut app = App::default();
        let mut terminal = Terminal::new(TestBackend::new(width, 24)).unwrap();
        terminal
            .draw(|frame| render::draw(frame, &mut app, &data))
            .unwrap();
        let rendered = text(&terminal);
        let line = rendered
            .lines()
            .find(|line| line.contains("Playback failed"))
            .unwrap_or_else(|| panic!("{width}\n{rendered}"));
        assert!(line.contains(" · ?"), "{width}: {line}");
        shown.push(line.matches(|c: char| c.is_ascii_alphanumeric()).count());
    }
    assert!(shown[0] < shown[1] && shown[1] < shown[2], "{shown:?}");
}

#[test]
fn recording_stages_show_a_timer_and_the_saved_file_in_the_header() {
    use crate::engine::RecordingState as R;
    let mut data = view();
    data.status.recording_ms = 65_400;
    data.status.recording_path = "/tmp/radio/Current Station-2026.mp3".into();
    for (state, expected) in [
        (R::Recording, "● REC 01:05"),
        (R::Finalizing, "… Finalizing recording · 01:05"),
        (
            R::Done,
            "Recording saved · 01:05 · Current Station-2026.mp3",
        ),
    ] {
        data.status.recording_state = state.clone();
        let mut app = App::default();
        let mut terminal = Terminal::new(TestBackend::new(120, 24)).unwrap();
        terminal
            .draw(|frame| render::draw(frame, &mut app, &data))
            .unwrap();
        let rendered = text(&terminal);
        assert!(rendered.contains(expected), "{state:?}\n{rendered}");
        assert!(!rendered.contains("/tmp/radio"), "{state:?}\n{rendered}");
    }
    // The compact layout keeps the timer within its two-line header.
    data.status.recording_state = R::Recording;
    let mut app = App::default();
    let mut terminal = Terminal::new(TestBackend::new(60, 20)).unwrap();
    terminal
        .draw(|frame| render::draw(frame, &mut app, &data))
        .unwrap();
    assert!(text(&terminal).contains("01:05"), "{}", text(&terminal));
}

#[test]
fn layout_snapshots_cover_standard_sizes_and_activity_expansion() {
    let mut data = view();
    // Keep fixtures independent of generated station identities and clock time.
    data.catalog_note = "Catalog ready".into();
    Arc::make_mut(&mut data.rows)[0].stationuuid = "00000000-0000-0000-0000-000000000000".into();
    for (width, height) in [(40, 16), (80, 24), (100, 24), (160, 48)] {
        for details in [false, true] {
            let mut app = App::default();
            if details {
                app.focus = Focus::Details;
            }
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal
                .draw(|frame| render::draw(frame, &mut app, &data))
                .unwrap();
            assert_eq!(app.rects[Focus::Activity.index()].height, 1);
            if width == 80 {
                assert!(app.rects[Focus::List.index()].height >= 13);
            }
            // UUID is displayed only in the details fixture.
            let rendered = text(&terminal).replace(
                &data.rows[0].stationuuid,
                "00000000-0000-0000-0000-000000000000",
            );
            assert_no_cjk(&rendered);
            let name = format!(
                "{}x{}-{}.txt",
                width,
                height,
                if details { "details" } else { "list" }
            );
            let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/snapshots")
                .join(name);
            if std::env::var_os("UPDATE_SNAPSHOTS").is_some() {
                std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                std::fs::write(&path, &rendered).unwrap();
            }
            assert_eq!(
                rendered,
                std::fs::read_to_string(&path).unwrap(),
                "{}",
                path.display()
            );
        }
    }
    data.task_active = true;
    let mut app = App::default();
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    terminal
        .draw(|frame| render::draw(frame, &mut app, &data))
        .unwrap();
    assert_eq!(app.rects[Focus::Activity.index()].height, 4);
}

#[test]
fn english_panels_and_editors_use_consistent_interface_language() {
    let mut data = view();
    data.task_active = true;
    data.task = crate::t!("catalog_sync_progress", "count" => 7);
    Arc::make_mut(&mut data.logs).push_back(crate::t!("catalog_sync_started").into());
    data.inspected_error = Some(crate::t!("station_details_failed", "detail" => "Database busy"));
    Arc::make_mut(&mut data.backups_errors).insert(
        data.rows[0].stationuuid.clone(),
        crate::t!("backups_query_failed", "detail" => "Database busy"),
    );
    data.recordings = vec![
        serde_json::from_value(serde_json::json!({
            "id":"saved", "stationuuid":"s", "station_name":"Saved audio",
            "source_url_resolved":"https://example.com", "started_at":"2026-01-01",
            "stopped_at":null, "media_duration_ms":null, "codec":"mp3", "container":"mp3",
            "path":"/tmp/sample", "size_bytes":1, "status":"failed", "error":"Disk error"
        }))
        .unwrap(),
    ]
    .into();
    for (width, height) in [(40, 16), (80, 24), (160, 48)] {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        for source in Source::ALL {
            for focus in Focus::ALL {
                let mut app = App::default();
                app.source = source;
                app.focus = focus;
                terminal.draw(|f| render::draw(f, &mut app, &data)).unwrap();
                assert_no_cjk(&text(&terminal));
            }
        }
        for mode in [Mode::Search, Mode::Command] {
            let mut app = App::default();
            app.mode = mode;
            terminal.draw(|f| render::draw(f, &mut app, &data)).unwrap();
            assert_no_cjk(&text(&terminal));
        }
        let mut app = App::default();
        app.help = true;
        terminal.draw(|f| render::draw(f, &mut app, &data)).unwrap();
        assert_no_cjk(&text(&terminal));
        app.help = false;
        app.filter_edit = Some((0, Default::default()));
        terminal.draw(|f| render::draw(f, &mut app, &data)).unwrap();
        assert_no_cjk(&text(&terminal));
        app.filter_edit = None;
        app.focus = Focus::Details;
        app.inspect_info = true;
        app.info_request = data.inspected_request;
        terminal.draw(|f| render::draw(f, &mut app, &data)).unwrap();
        assert_no_cjk(&text(&terminal));
        if width >= 80 {
            assert!(text(&terminal).contains("Details query failed"));
        }
    }
    data.status.station = None;
    data.status.title.clear();
    data.status.muted = true;
    for width in [40, 80, 160] {
        let mut app = App::default();
        let mut terminal = Terminal::new(TestBackend::new(width, 24)).unwrap();
        terminal.draw(|f| render::draw(f, &mut app, &data)).unwrap();
        assert_no_cjk(&text(&terminal));
    }
}

#[test]
fn themes_keep_text_and_focus_legible_at_eighty_columns() {
    use crate::config::Theme;
    let data = view();
    let mut reference = None;
    for theme in [Theme::Default, Theme::Mono, Theme::Light] {
        let mut app = App::default();
        app.theme = theme;
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal
            .draw(|frame| render::draw(frame, &mut app, &data))
            .unwrap();
        let rendered = text(&terminal);
        assert!(rendered.contains("▶ [2/3]"));
        if let Some(reference) = &reference {
            assert_eq!(&rendered, reference);
        } else {
            reference = Some(rendered);
        }
        for cell in &terminal.backend().buffer().content {
            match theme {
                Theme::Mono => assert_eq!((cell.fg, cell.bg), (Color::Reset, Color::Reset)),
                Theme::Light if !cell.symbol().trim().is_empty() => assert_ne!(cell.fg, cell.bg),
                _ => {}
            }
        }
        let selected = &terminal.backend().buffer()[(
            app.rects[Focus::List.index()].x + 1,
            app.rects[Focus::List.index()].y + 1,
        )];
        if theme == Theme::Light {
            assert_eq!((selected.fg, selected.bg), (Color::White, Color::Blue));
        }
    }
    assert_eq!(Theme::Light.effective(true), Theme::Mono);
    assert_eq!(Theme::Light.effective(false), Theme::Light);
}

#[test]
fn every_media_stage_is_visible_and_failures_have_details() {
    use crate::engine::{PlaybackState as P, RecordingState as R};
    let mut data = view();
    for playback in [
        P::Idle,
        P::Connecting,
        P::Reconnecting { attempt: 2 },
        P::Playing,
        P::Paused,
        P::Failed {
            reason: "HTTP unavailable".into(),
        },
    ] {
        for recording in [
            R::Idle,
            R::Probing,
            R::Recording,
            R::Finalizing,
            R::Done,
            R::Failed {
                reason: "FFmpeg: No space left on device".into(),
            },
        ] {
            data.status.playback_state = playback.clone();
            data.status.recording_state = recording.clone();
            let mut app = App::default();
            let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
            terminal
                .draw(|frame| render::draw(frame, &mut app, &data))
                .unwrap();
            let rendered = text(&terminal);
            assert_no_cjk(&rendered);
            assert!(rendered.contains(playback.label()), "{rendered}");
            assert!(rendered.contains(recording.label()), "{rendered}");
        }
    }
    let mut app = App::default();
    let (sender, _) = command::channel(4);
    app.key(key(KeyCode::Char('?')), &data, &sender);
    assert!(
        app.help_content
            .as_deref()
            .unwrap()
            .contains("No space left on device")
    );
    assert!(
        app.help_content
            .as_deref()
            .unwrap()
            .contains("HTTP unavailable")
    );
    assert_eq!(
        unicode_width::UnicodeWidthStr::width(render::fit("中文超长name", 5).as_str()),
        5
    );
    assert!(render::fit("中文超长name", 5).contains('…'));
}

#[test]
fn view_snapshots_share_partitions_and_skip_unchanged_publications() {
    let mut data = view();
    let first = data.snapshot();
    assert!(Arc::ptr_eq(&first, &data.snapshot()));
    data.status.volume = 42;
    data.revision += 1;
    let second = data.snapshot();
    assert!(!Arc::ptr_eq(&first, &second));
    assert!(Arc::ptr_eq(&first.rows, &second.rows));
    assert!(Arc::ptr_eq(&first.recordings, &second.recordings));
    assert!(Arc::ptr_eq(&first.logs, &second.logs));
    assert_eq!(first.rows_revision, second.rows_revision);
    let mut status = data.status.clone();
    status.recording_ms = 900;
    assert!(data.status.ui_eq(&status));
    status.recording_ms = 1000;
    assert!(!data.status.ui_eq(&status));
    assert!(data.status.control_eq(&status));
    status.volume = 43;
    assert!(!data.status.control_eq(&status));
}

#[test]
fn station_refresh_does_not_read_recordings_until_that_source_is_selected() {
    let dir = tempfile::tempdir().unwrap();
    let config = crate::test_support::config(dir.path());
    let mut engine = Engine::new(config, Arc::new(AtomicBool::new(false))).unwrap();
    engine.store.upsert(&station("lazy")).unwrap();
    rusqlite::Connection::open(dir.path().join("radio.db")).unwrap()
        .execute("INSERT INTO recordings(id,status,json,recovery_pending) VALUES ('broken','complete','not json',0)",[]).unwrap();
    let data = Arc::new(Mutex::new(View::new(engine.status.clone())));
    let mut filter = Filter {
        limit: 100,
        ..Default::default()
    };
    let mut job = None;
    worker::execute(
        Command::Search(filter.clone()),
        &mut engine,
        &mut filter,
        &mut job,
        &data,
    )
    .unwrap();
    worker::refresh_if_needed(&engine, &filter, &data, false).unwrap();
    assert_eq!(data.lock().unwrap().rows.len(), 1);
    assert_eq!(data.lock().unwrap().recordings_revision, 0);
    worker::execute(
        Command::Recordings(filter.clone()),
        &mut engine,
        &mut filter,
        &mut job,
        &data,
    )
    .unwrap();
    worker::refresh_if_needed(&engine, &filter, &data, false).unwrap();
    assert!(data.lock().unwrap().page_error(&filter).is_some());
}

#[test]
fn filter_popup_reuses_unicode_cursor_and_word_editing() {
    let mut app = App::default();
    let data = view();
    let (sender, _) = command::channel(10);
    app.key(key(KeyCode::Char('F')), &data, &sender);
    app.key(key(KeyCode::Tab), &data, &sender);
    for c in "中文 jazz".chars() {
        app.key(key(KeyCode::Char(c)), &data, &sender);
    }
    app.key(key(KeyCode::Home), &data, &sender);
    app.key(key(KeyCode::Right), &data, &sender);
    app.key(key(KeyCode::Delete), &data, &sender);
    assert_eq!(app.filter_edit.as_ref().unwrap().1[1], "中 jazz");
    app.key(key(KeyCode::End), &data, &sender);
    app.key(
        KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL),
        &data,
        &sender,
    );
    assert_eq!(app.filter_edit.as_ref().unwrap().1[1], "中 ");
    app.key(key(KeyCode::Enter), &data, &sender);
    assert_eq!(app.filter.language.as_deref(), Some("中"));
}

#[test]
fn command_completions_are_scoped_to_arguments_and_selected_items() {
    let stations = vec!["selected-uuid".into()];
    let candidates = command::completion_candidates("record ", &stations, &[]);
    assert!(candidates.contains(&"record selected-uuid".into()));
    assert!(candidates.contains(&"record --duration".into()));
    assert!(!candidates.iter().any(|s| s.contains("--country")));
    let add = command::completion_candidates("station add ", &[], &[]);
    assert!(add.iter().any(|s| s.contains("https://")));
    let backup =
        command::completion_candidates("station add '中文 name' https://example.com --b", &[], &[]);
    assert_eq!(
        backup,
        vec!["station add '中文 name' https://example.com --backup-url"]
    );
    assert_eq!(
        command::completion_candidates("record --duration ", &stations, &[]),
        vec![
            "record --duration 10m",
            "record --duration 30m",
            "record --duration 1h"
        ]
    );
    assert!(
        matches!(command::parse("help record",None,None,&Filter::default()).unwrap(),Parsed::HelpText(text) if text.contains("--duration"))
    );
    assert!(command::parse("set language invalid", None, None, &Filter::default()).is_err());
}

#[test]
fn command_history_survives_sessions_with_private_bounded_storage() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("command-history.json");
    let mut editor = Editor::default();
    editor.load_history(path.clone()).unwrap();
    for i in 0..105 {
        editor.set(&format!("search station {i}"));
        editor.remember().unwrap();
    }
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let saved: Vec<String> = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(saved.len(), 100);
    assert_eq!(saved[0], "search station 5");
    let mut next = Editor::default();
    next.load_history(path).unwrap();
    next.begin("draft");
    next.key(key(KeyCode::Up), true);
    assert_eq!(next.text, "search station 104");
    next.key(key(KeyCode::Down), true);
    assert_eq!(next.text, "draft");
}

#[test]
fn commands_with_url_credentials_stay_out_of_the_command_history() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("command-history.json");
    let data = view();
    let (sender, receiver) = command::channel(16);
    let mut app = App::default();
    app.editor.load_history(path.clone()).unwrap();
    let run = |app: &mut App, line: &str| {
        app.key(key(KeyCode::Char(':')), &data, &sender);
        app.editor.set(line);
        app.key(key(KeyCode::Enter), &data, &sender);
    };

    run(&mut app, "station add Plain https://example.com/live");
    assert_eq!(app.message, crate::t!("command_submitted"));
    let kept = std::fs::read(&path).unwrap();

    for line in [
        "station add Secret https://user:hunter2@example.com/live",
        "station add Signed 'https://example.com/live?quality=high&token=hunter2'",
    ] {
        run(&mut app, line);
        // The command itself still runs; only the history skips it.
        assert!(
            matches!(receiver.try_recv(), Ok(Command::Add { .. })),
            "{line}"
        );
        assert_eq!(
            app.message,
            format!(
                "{} · {}",
                crate::t!("command_submitted"),
                crate::t!("history_withheld")
            ),
            "{line}"
        );
        assert_eq!(std::fs::read(&path).unwrap(), kept, "{line}");
        app.key(key(KeyCode::Char(':')), &data, &sender);
        app.key(key(KeyCode::Up), &data, &sender);
        assert_eq!(
            app.editor.text,
            "station add Plain https://example.com/live"
        );
        app.key(key(KeyCode::Esc), &data, &sender);
    }
    assert!(matches!(receiver.try_recv(), Ok(Command::Add { .. })));
    assert!(!String::from_utf8_lossy(&kept).contains("hunter2"));

    run(&mut app, "play https://user:hunter2@example.com/live");
    assert!(
        matches!(receiver.try_recv(), Ok(Command::Play(url)) if url == "https://user:hunter2@example.com/live")
    );
    assert_eq!(
        app.message,
        format!(
            "{} · {}",
            crate::t!("command_submitted"),
            crate::t!("history_withheld")
        )
    );
    assert_eq!(std::fs::read(&path).unwrap(), kept);

    // A failure notice is kept and the reason is added after it.
    let (full, _held) = command::channel(1);
    app.key(key(KeyCode::Char(':')), &data, &full);
    app.editor
        .set("station add Again https://u:p@example.com/live");
    app.key(key(KeyCode::Enter), &data, &full);
    app.key(key(KeyCode::Char(':')), &data, &full);
    app.editor
        .set("station add More https://u:p@example.com/live");
    app.key(key(KeyCode::Enter), &data, &full);
    assert!(app.message.ends_with(crate::t!("history_withheld")));
    assert_ne!(app.message, crate::t!("history_withheld"));
}

#[test]
fn copying_a_long_url_reports_truncation_only_above_the_byte_limit() {
    let mut data = view();
    let mut app = App::default();
    let (sender, _) = command::channel(4);
    for length in [4095, 4096, 4097] {
        let station = &mut Arc::make_mut(&mut data.rows)[0];
        station.url = format!("https://host/{}", "x".repeat(length - 13));
        station.url_resolved = station.url.clone();
        let name = station.name.clone();
        app.key(key(KeyCode::Char('Y')), &data, &sender);
        let url = app.clipboard.as_ref().unwrap();
        assert_eq!(url.len(), length);
        let notice = crate::t!("clipboard_station_url", "name" => name);
        let expected = if length > 4096 {
            crate::t!("clipboard_truncated", "notice" => notice)
        } else {
            notice
        };
        assert_eq!(app.message, expected);
    }
}

#[test]
fn copy_notices_name_the_row_that_was_copied() {
    let mut data = view();
    let mut app = App::default();
    let (sender, _receiver) = command::channel(10);
    let selected = data.rows[0].clone();

    app.key(key(KeyCode::Char('y')), &data, &sender);
    assert_eq!(
        app.clipboard.as_deref(),
        Some(selected.stationuuid.as_str())
    );
    assert_eq!(
        app.message,
        crate::t!("clipboard_station_uuid", "name" => "Selected Station")
    );
    app.key(key(KeyCode::Char('Y')), &data, &sender);
    assert_eq!(app.clipboard.as_deref(), Some(selected.primary_url()));
    assert_eq!(
        app.message,
        crate::t!("clipboard_station_url", "name" => "Selected Station")
    );
    // The row is the target, not the station that happens to be playing.
    assert!(!app.message.contains("Current Station"));

    Arc::make_mut(&mut data.rows)[0].name = "  ".into();
    app.key(key(KeyCode::Char('y')), &data, &sender);
    assert!(
        app.message.ends_with(&selected.stationuuid),
        "{}",
        app.message
    );

    data.recordings = vec![
        serde_json::from_value(serde_json::json!({
            "id":"saved", "stationuuid":"s", "station_name":"Saved audio",
            "source_url_resolved":"https://example.com", "started_at":"2026-01-01",
            "stopped_at":null, "media_duration_ms":null, "codec":"mp3", "container":"mp3",
            "path":"/tmp/sample", "size_bytes":1, "status":"done", "error":null,
            "reconnects": 0
        }))
        .unwrap(),
    ]
    .into();
    app.source = Source::Recordings;
    app.key(key(KeyCode::Char('y')), &data, &sender);
    assert_eq!(app.clipboard.as_deref(), Some("saved"));
    assert_eq!(
        app.message,
        crate::t!("clipboard_recording_id", "name" => "Saved audio")
    );
    app.key(key(KeyCode::Char('Y')), &data, &sender);
    assert_eq!(app.clipboard.as_deref(), Some("/tmp/sample"));
    assert_eq!(
        app.message,
        crate::t!("clipboard_recording_path", "name" => "Saved audio")
    );
}

#[test]
fn favorite_log_entries_name_the_station() {
    let dir = tempfile::tempdir().unwrap();
    let config = crate::test_support::config(dir.path());
    let mut engine = Engine::new(config, Arc::new(AtomicBool::new(false))).unwrap();
    let row = station("Jazz FM");
    engine.store.upsert(&row).unwrap();
    let shared = Arc::new(Mutex::new(View::new(engine.status.clone())));
    let mut filter = Filter {
        limit: 100,
        ..Filter::default()
    };
    let mut job = None;
    let uuid = row.stationuuid.clone();
    for (command, expected) in [
        (
            Command::ToggleFavorite(uuid.clone()),
            crate::t!("station_favorited", "name" => "Jazz FM"),
        ),
        (
            Command::ToggleFavorite(uuid.clone()),
            crate::t!("station_unfavorited", "name" => "Jazz FM"),
        ),
        (
            Command::Favorite(uuid.clone(), true),
            crate::t!("station_favorited", "name" => "Jazz FM"),
        ),
    ] {
        worker::execute(command, &mut engine, &mut filter, &mut job, &shared).unwrap();
        let data = shared.lock().unwrap();
        let last = data.logs.back().unwrap();
        assert!(last.ends_with(&expected), "{last} vs {expected}");
    }
}

#[test]
fn mute_copy_and_mouse_setting_route_without_shell_execution() {
    let data = view();
    let mut app = App::default();
    let (sender, receiver) = command::channel(10);
    app.key(key(KeyCode::Char('m')), &data, &sender);
    assert!(matches!(receiver.try_recv(), Ok(Command::Mute)));
    app.key(key(KeyCode::Char('y')), &data, &sender);
    assert_eq!(
        app.clipboard.as_deref(),
        Some(data.rows[0].stationuuid.as_str())
    );
    app.key(key(KeyCode::Char(':')), &data, &sender);
    app.editor.set("set mouse false");
    app.key(key(KeyCode::Enter), &data, &sender);
    assert!(!app.mouse_enabled);
    assert!(
        matches!(command::parse("set language en",None,None,&Filter::default()).unwrap(),Parsed::Worker(Command::SetLanguage(language)) if language == "en")
    );
}

#[test]
fn light_popups_keep_the_cleared_border_column_on_the_light_background() {
    let data = view();
    let mut app = App::default();
    app.theme = crate::config::Theme::Light;
    app.help = true;
    let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
    terminal.draw(|f| render::draw(f, &mut app, &data)).unwrap();
    // The popup clears one extra column to avoid wide-character diff skips.
    // That column must inherit the theme background too.
    let buffer = terminal.backend().buffer();
    assert!(
        buffer
            .content
            .iter()
            .all(|cell| cell.bg == ratatui::style::Color::White
                || cell.bg == ratatui::style::Color::Blue)
    );
}

#[test]
fn viewport_alignment_survives_render_resize_and_mouse_hits() {
    use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
    let (sender, receiver) = command::channel(4);
    for source in Source::ALL {
        for len in [3, 40] {
            for selected in [0, len - 1] {
                for suffix in ['t', 'z', 'b'] {
                    let (mut app, data) = scrollable_list(source, len);
                    app.list.select(Some(selected));
                    // Reuse state across resizes, including a short viewport.
                    for (width, height) in [(80, 24), (80, 16), (120, 40)] {
                        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
                        terminal.draw(|f| render::draw(f, &mut app, &data)).unwrap();
                        app.list.select(Some(selected));
                        let rect = app.rects[Focus::List.index()];
                        let height = usize::from(rect.height - 2);
                        let above = match suffix {
                            't' => 0,
                            'b' => height - 1,
                            _ => (height - 1) / 2,
                        };
                        let offset = selected.saturating_sub(above);
                        app.key(key(KeyCode::Char('z')), &data, &sender);
                        app.key(key(KeyCode::Char(suffix)), &data, &sender);
                        terminal.draw(|f| render::draw(f, &mut app, &data)).unwrap();
                        assert_eq!(app.list.offset(), offset, "{source:?}, {len}, z{suffix}");
                        assert_eq!(app.list.selected(), Some(selected));
                        let first_row: String = (rect.x + 1..rect.right() - 1)
                            .map(|x| terminal.backend().buffer()[(x, rect.y + 1)].symbol())
                            .collect();
                        assert!(
                            first_row.contains(&format!("Row {offset:03}")),
                            "{first_row}"
                        );
                        let visible = height.min(len - offset);
                        for (c, index) in [
                            ('H', offset),
                            ('M', offset + (visible - 1) / 2),
                            ('L', offset + visible - 1),
                        ] {
                            app.key(key(KeyCode::Char(c)), &data, &sender);
                            terminal.draw(|f| render::draw(f, &mut app, &data)).unwrap();
                            assert_eq!(app.list.selected(), Some(index));
                            assert_eq!(app.list.offset(), offset);
                        }
                        for row in 0..height {
                            let previous = app.list.selected();
                            app.mouse_at(
                                MouseEvent {
                                    kind: MouseEventKind::Down(MouseButton::Left),
                                    column: rect.x + 3,
                                    row: rect.y + 1 + row as u16,
                                    modifiers: KeyModifiers::NONE,
                                },
                                &data,
                                &sender,
                                Instant::now() + Duration::from_secs(row as u64),
                            );
                            let expected = if row < visible {
                                Some(offset + row)
                            } else {
                                previous
                            };
                            assert_eq!(app.list.selected(), expected);
                            terminal.draw(|f| render::draw(f, &mut app, &data)).unwrap();
                            assert_eq!(app.list.offset(), offset);
                        }
                    }
                }
            }
        }
    }
    assert!(receiver.try_recv().is_err());
}

#[test]
fn navigation_prefixes_cancel_on_another_prefix_or_control_key() {
    let (sender, receiver) = command::channel(4);
    for sequence in ["gzt", "gzz", "gzb"] {
        let (mut app, data) = scrollable_list(Source::Stations, 40);
        app.rects[Focus::List.index()] = ratatui::layout::Rect::new(0, 0, 80, 12);
        app.list.select(Some(15));
        *app.list.offset_mut() = 10;
        for c in sequence.chars() {
            app.key(key(KeyCode::Char(c)), &data, &sender);
        }
        assert_eq!(app.list.offset(), 10, "{sequence}");
        assert_eq!(app.list.selected(), Some(15), "{sequence}");
    }
    for (prefix, suffix) in [('g', 'g'), ('z', 't')] {
        let (mut app, data) = scrollable_list(Source::Stations, 40);
        app.rects[Focus::List.index()] = ratatui::layout::Rect::new(0, 0, 80, 12);
        app.list.select(Some(15));
        *app.list.offset_mut() = 10;
        app.key(key(KeyCode::Char(prefix)), &data, &sender);
        app.key(
            KeyEvent::new(KeyCode::Char('x'), KeyModifiers::CONTROL),
            &data,
            &sender,
        );
        app.key(key(KeyCode::Char(suffix)), &data, &sender);
        assert_eq!(app.list.offset(), 10, "{prefix}, Ctrl-x, {suffix}");
        assert_eq!(app.list.selected(), Some(15), "{prefix}, Ctrl-x, {suffix}");
    }
    assert!(receiver.try_recv().is_err());
}

#[test]
fn viewport_keys_use_visible_rows_and_preserve_selection_when_aligning() {
    let mut data = view();
    data.rows = Arc::new((0..30).map(|n| station(&format!("Row {n}"))).collect());
    let mut app = App::default();
    app.rects[Focus::List.index()] = ratatui::layout::Rect::new(0, 0, 80, 12);
    *app.list.offset_mut() = 8;
    let (sender, _receiver) = command::channel(4);
    for (key_char, expected) in [('H', 8), ('M', 12), ('L', 17)] {
        app.key(key(KeyCode::Char(key_char)), &data, &sender);
        assert_eq!(app.list.selected(), Some(expected));
    }
    for (suffix, expected) in [('t', 17), ('z', 13), ('b', 8)] {
        app.key(key(KeyCode::Char('z')), &data, &sender);
        app.key(key(KeyCode::Char(suffix)), &data, &sender);
        assert_eq!(app.list.offset(), expected);
        assert_eq!(app.list.selected(), Some(17));
    }
    // Short final viewport and a resized-away list stay bounded.
    *app.list.offset_mut() = 28;
    app.key(key(KeyCode::Char('L')), &data, &sender);
    assert_eq!(app.list.selected(), Some(29));
    app.rects[Focus::List.index()].height = 0;
    app.key(key(KeyCode::Char('H')), &data, &sender);
    assert_eq!(app.list.selected(), Some(29));
    // Configured locate sequences consume their keys before viewport actions.
    app.jump_binding = "zz".into();
    app.key(key(KeyCode::Char('z')), &data, &sender);
    app.key(key(KeyCode::Char('z')), &data, &sender);
    assert!(matches!(
        _receiver.try_recv(),
        Ok(Command::LocatePlaying { .. })
    ));
}

#[test]
fn scrolling_logs_keeps_the_visible_entries_as_messages_arrive() {
    let mut data = view();
    data.logs = Arc::new(
        (1..=20)
            .map(|n| Arc::from(format!("entry-{n:03}")))
            .collect(),
    );
    data.logs_revision = 20;
    let mut app = App::default();
    app.focus = Focus::Activity;
    app.activity_scroll = 5;
    let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
    terminal.draw(|f| render::draw(f, &mut app, &data)).unwrap();
    assert!(text(&terminal).contains("entry-015"));
    Arc::make_mut(&mut data.logs).push_back(Arc::from("entry-021"));
    data.logs_revision += 1;
    terminal.draw(|f| render::draw(f, &mut app, &data)).unwrap();
    let screen = text(&terminal);
    assert!(screen.contains("entry-015"));
    assert!(!screen.contains("entry-021"));
    assert!(screen.contains(&crate::t!("activity_new", "count" => 1)));
    data.logs = Arc::new(
        (100..=299)
            .map(|n| Arc::from(format!("entry-{n:03}")))
            .collect(),
    );
    data.logs_revision = 299;
    terminal.draw(|f| render::draw(f, &mut app, &data)).unwrap();
    assert!(text(&terminal).contains(crate::t!("activity_removed")));
    assert!(text(&terminal).contains("entry-100"));
    app.activity_scroll = 0;
    terminal.draw(|f| render::draw(f, &mut app, &data)).unwrap();
    assert!(text(&terminal).contains("entry-299"));
    assert!(!text(&terminal).contains(crate::t!("activity_removed")));
    assert_eq!(app.activity_new, 0);
}

#[test]
fn artwork_checks_reuse_details_wrapping_until_content_or_width_changes() {
    let mut data = view();
    let app = App::default();
    let row = data.rows[0].clone();
    for _ in 0..100 {
        render::station_lines(&app, &row, &data, 60);
    }
    assert_eq!(app.details_cache.borrow().rebuilds, 1);
    render::station_lines(&app, &row, &data, 40);
    assert_eq!(app.details_cache.borrow().rebuilds, 2);
    Arc::make_mut(&mut data.backups)
        .insert(row.stationuuid.clone(), vec!["https://backup/live".into()]);
    let lines = render::station_lines(&app, &row, &data, 40);
    assert_eq!(app.details_cache.borrow().rebuilds, 3);
    assert!(
        lines
            .iter()
            .any(|line| line.to_string().contains("https://backup/live"))
    );
    let mut changed = row;
    changed.name = "New name".into();
    render::station_lines(&app, &changed, &data, 40);
    assert_eq!(app.details_cache.borrow().rebuilds, 4);
}

#[test]
fn details_name_the_origin_and_backup_request_phase() {
    let mut data = view();
    let mut app = App::default();
    let row = data.rows[0].clone();
    let details = render::station_details(&app, &row, &data);
    assert!(details.contains(crate::t!("details_origin_selected")));
    assert!(details.contains(crate::t!("backups_not_requested")));
    let (sender, _) = command::channel(4);
    app.request_selected_backups(&data, &sender);
    // A disconnected queue must not claim a pending request.
    assert!(
        render::station_details(&app, &row, &data).contains(crate::t!("backups_not_requested"))
    );
    let (sender, _receiver) = command::channel(4);
    app.request_selected_backups(&data, &sender);
    assert!(render::station_details(&app, &row, &data).contains(crate::t!("backups_loading")));
    Arc::make_mut(&mut data.backups).insert(row.stationuuid.clone(), vec![]);
    assert!(render::station_details(&app, &row, &data).contains(crate::t!("backups_none")));
    Arc::make_mut(&mut data.backups_errors).insert(row.stationuuid.clone(), "query failed".into());
    assert!(render::station_details(&app, &row, &data).contains("query failed"));
    for (line, origin) in [
        (
            format!("info {}", row.stationuuid),
            crate::t!("details_origin_info"),
        ),
        ("current".into(), crate::t!("details_origin_playing")),
    ] {
        app.key(key(KeyCode::Char(':')), &data, &sender);
        app.editor.set(&line);
        app.key(key(KeyCode::Enter), &data, &sender);
        assert!(render::station_details(&app, &row, &data).contains(origin));
    }
}

#[test]
fn backup_continuations_do_not_insert_blank_labels() {
    let urls = ["https://a.test/one", "https://b.test/two"];
    let content = format!("Backups:\t{}\n\t{}", urls[0], urls[1]);
    for width in [12, 38, 44, 60] {
        let lines: Vec<_> = render::field_lines(&content, width)
            .iter()
            .map(ToString::to_string)
            .collect();
        assert!(
            lines.iter().all(|line| !line.trim().is_empty()),
            "{width}: {lines:?}"
        );
        let values: String = lines
            .iter()
            .map(|line| line.strip_prefix("Backups:").unwrap_or(line).trim())
            .collect();
        assert_eq!(values, urls.concat(), "{width}");
    }
}

#[test]
fn station_fields_align_or_stack_and_keep_the_whole_address() {
    let url = "https://example.com/very/long/path?quality=high&language=中文&another=value";
    let content = format!("Catalog\nCountry:\tFrance\nLanguage:\tFrench\n\nAddresses\nURL:\t{url}");
    for width in [12, 38, 60, 100] {
        let lines = render::field_lines(&content, width);
        let lines: Vec<_> = lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect();
        assert!(
            lines.iter().all(|line| super::text::width(line) <= width),
            "{width}: {lines:?}"
        );
        let from = lines
            .iter()
            .position(|line| line.starts_with("URL:"))
            .unwrap();
        let fragments: String = lines[from..]
            .iter()
            .map(|line| line.strip_prefix("URL:").unwrap_or(line).trim())
            .collect();
        assert_eq!(fragments, url, "{width}");
        if width < 44 {
            assert_eq!(lines[1], "Country:");
            assert_eq!(lines[2], "  France");
        } else {
            assert_eq!(lines[1].find("France"), lines[2].find("French"));
        }
    }
    let lines = render::field_lines("Title\x1b[2J\nU\x1b[31mUID:\tbad\x1b[31m-id\x07", 60);
    let rendered = lines.iter().map(ToString::to_string).collect::<String>();
    assert!(!rendered.contains('\x1b'));
    assert!(!rendered.contains('\x07'));
    assert!(rendered.contains("bad[31m-id"));
}

#[test]
fn diagnostics_hide_url_credentials_without_changing_the_playback_url() {
    let mut data = view();
    let url = "https://user:private@host/live?token=confidential&quality=high";
    data.status.source_url = url.into();
    data.status.playback_state = crate::engine::PlaybackState::Failed {
        reason: format!("failed to open {url}"),
    };
    data.operation_error_id = 1;
    data.operation_error = format!("request failed: {url}");
    data.task = format!("sync failed: {url}");
    let mut app = App::default();
    let (sender, _) = command::channel(4);
    let mut terminal = Terminal::new(TestBackend::new(160, 48)).unwrap();
    terminal.draw(|f| render::draw(f, &mut app, &data)).unwrap();
    let screen = text(&terminal);
    assert!(!screen.contains("private"));
    assert!(!screen.contains("confidential"));
    assert!(screen.contains("[redacted]"));
    app.key(key(KeyCode::Char('?')), &data, &sender);
    terminal.draw(|f| render::draw(f, &mut app, &data)).unwrap();
    let screen = text(&terminal);
    assert!(!screen.contains("private"));
    assert!(!screen.contains("confidential"));
    assert!(screen.contains("host/live?token=[redacted]&quality=high"));
    assert_eq!(data.status.source_url, url);
    assert!(data.operation_error.contains("private"));
}

#[test]
fn failure_details_sanitize_external_control_text_and_scroll_the_full_tail() {
    let mut data = view();
    data.status.recording_state = crate::engine::RecordingState::Failed {
        reason: format!(
            "\x1b]0;injected\x07\n{}\nlast FFmpeg failure",
            "detail line\n".repeat(150)
        ),
    };
    let mut app = App::default();
    let (sender, _) = command::channel(4);
    app.key(key(KeyCode::Char('?')), &data, &sender);
    let mut initial = Terminal::new(TestBackend::new(80, 24)).unwrap();
    initial
        .draw(|frame| render::draw(frame, &mut app, &data))
        .unwrap();
    assert!(!text(&initial).contains('\x1b'));
    assert!(!text(&initial).contains('\x07'));
    for _ in 0..30 {
        app.key(key(KeyCode::PageDown), &data, &sender);
    }
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    terminal
        .draw(|frame| render::draw(frame, &mut app, &data))
        .unwrap();
    let rendered = text(&terminal);
    assert!(!rendered.contains('\x1b'));
    assert!(!rendered.contains('\x07'));
    assert!(rendered.contains("last FFmpeg failure"));

    data.inspected_request = 7;
    data.inspected_error = Some(format!(
        "\x1b]0;injected\x07\n{}\nlast details failure",
        "detail line\n".repeat(150)
    ));
    let mut app = App::default();
    app.focus = Focus::Details;
    app.inspect_info = true;
    app.info_request = 7;
    terminal.draw(|f| render::draw(f, &mut app, &data)).unwrap();
    assert!(!text(&terminal).contains('\x1b'));
    assert!(!text(&terminal).contains('\x07'));
    assert!(!text(&terminal).contains("last details failure"));
    app.details_scroll = 4096;
    terminal.draw(|f| render::draw(f, &mut app, &data)).unwrap();
    assert!(app.details_scroll > 100);
    assert!(text(&terminal).contains("last details failure"));
}

#[test]
fn owner_coalesces_refreshes_and_loads_only_selected_backups() {
    let dir = tempfile::tempdir().unwrap();
    let config = crate::test_support::config(dir.path());
    let mut engine = Engine::new(config, Arc::new(AtomicBool::new(false))).unwrap();
    let rows = [station("A"), station("B")];
    for row in &rows {
        engine.store.upsert(row).unwrap();
        engine
            .store
            .add_backup(&row.stationuuid, "https://example.com/backup")
            .unwrap();
    }
    let data = Arc::new(Mutex::new(View::new(engine.status.clone())));
    let mut filter = Filter {
        limit: 100,
        ..Filter::default()
    };
    let mut job = None;
    worker::execute(
        Command::Search(filter.clone()),
        &mut engine,
        &mut filter,
        &mut job,
        &data,
    )
    .unwrap();
    worker::execute(Command::Refresh, &mut engine, &mut filter, &mut job, &data).unwrap();
    assert_eq!(data.lock().unwrap().rows_revision, 0);
    worker::refresh_if_needed(&engine, &filter, &data, false).unwrap();
    assert_eq!(data.lock().unwrap().rows_revision, 1);
    assert!(data.lock().unwrap().backups.is_empty());
    worker::refresh_if_needed(&engine, &filter, &data, false).unwrap();
    assert_eq!(data.lock().unwrap().rows_revision, 1);
    let revision = engine.store.revision().unwrap();
    engine.store.set_meta("pref_source", "stations").unwrap();
    assert_eq!(engine.store.revision().unwrap(), revision);
    let snapshot = data.lock().unwrap().snapshot();
    let (tx, rx) = command::channel(10);
    let mut app = App::default();
    app.reconcile(&snapshot);
    app.request_selected_backups(&snapshot, &tx);
    app.request_selected_backups(&snapshot, &tx);
    let command = rx.try_recv().unwrap();
    assert!(matches!(&command, Command::Backups(id) if id == &snapshot.rows[0].stationuuid));
    assert!(rx.try_recv().is_err());
    worker::execute(command, &mut engine, &mut filter, &mut job, &data).unwrap();
    let snapshot = data.lock().unwrap().snapshot();
    assert_eq!(snapshot.backups.len(), 1);
    assert_eq!(
        snapshot.backups[&snapshot.rows[0].stationuuid],
        ["https://example.com/backup"]
    );
}

#[test]
fn facet_candidates_follow_the_typed_prefix_without_losing_it() {
    let mut data = view();
    Arc::make_mut(&mut data.facets)[0] = ["AU", "CA", "CN", "FR"]
        .into_iter()
        .map(|value| crate::model::Facet {
            value: value.into(),
            count: 1,
        })
        .collect();
    Arc::make_mut(&mut data.facets)[1] = (0..12)
        .map(|i| crate::model::Facet {
            value: format!("english{i:02}"),
            count: i + 1,
        })
        .collect();
    let (sender, _) = command::channel(10);
    let mut app = App::default();
    app.key(key(KeyCode::Char('F')), &data, &sender);
    app.key(key(KeyCode::Char('c')), &data, &sender);
    assert_eq!(app.facet_candidates(&data, 0).len(), 2);
    app.key(key(KeyCode::Down), &data, &sender);
    assert_eq!(app.filter_edit.as_ref().unwrap().1[0], "CA");
    app.key(key(KeyCode::Down), &data, &sender);
    assert_eq!(app.filter_edit.as_ref().unwrap().1[0], "CN");
    assert_eq!(app.facet_prefix, "c");
    app.key(key(KeyCode::Tab), &data, &sender);
    for c in "eng".chars() {
        app.key(key(KeyCode::Char(c)), &data, &sender);
    }
    assert_eq!(app.facet_candidates(&data, 1).len(), 12);
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    terminal.draw(|f| render::draw(f, &mut app, &data)).unwrap();
    assert!(text(&terminal).contains("english07 (8)"));
    assert!(!text(&terminal).contains("english08 (9)"));
    for _ in 0..12 {
        app.key(key(KeyCode::Down), &data, &sender);
    }
    terminal.draw(|f| render::draw(f, &mut app, &data)).unwrap();
    assert!(text(&terminal).contains("english04 (5)"));
    assert!(text(&terminal).contains("english11 (12)"));
    Arc::make_mut(&mut data.facets)[1].drain(..4);
    app.reconcile(&data);
    assert_eq!(
        app.facet_index,
        Some(7),
        "async results must keep the selected value aligned"
    );
    app.key(key(KeyCode::Char('z')), &data, &sender);
    assert!(app.facet_candidates(&data, 1).is_empty());
}

#[test]
fn short_wide_player_keeps_the_title_visible() {
    let mut data = view();
    data.status.title = "VISIBLE_TITLE".into();
    for height in 16..20 {
        let mut app = App::default();
        let mut terminal = Terminal::new(TestBackend::new(100, height)).unwrap();
        terminal.draw(|f| render::draw(f, &mut app, &data)).unwrap();
        assert!(text(&terminal).contains("VISIBLE_TITLE"), "height={height}");
    }
}

#[test]
fn muted_state_is_visible_without_color() {
    let mut data = view();
    data.status.muted = true;
    let mut app = App::default();
    app.theme = crate::config::Theme::Mono;
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    terminal.draw(|f| render::draw(f, &mut app, &data)).unwrap();
    assert!(text(&terminal).contains(crate::t!("player_muted")));
}

#[test]
fn narrow_details_focus_and_recording_random_match_the_visible_source() {
    let data = view();
    let mut app = App::default();
    let (tx, rx) = command::channel(10);
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    terminal.draw(|f| render::draw(f, &mut app, &data)).unwrap();
    app.key(
        KeyEvent::new(KeyCode::Char('l'), KeyModifiers::CONTROL),
        &data,
        &tx,
    );
    assert_eq!(app.focus, Focus::Details);
    app.key(
        KeyEvent::new(KeyCode::Char('h'), KeyModifiers::CONTROL),
        &data,
        &tx,
    );
    assert_eq!(app.focus, Focus::List);
    app.source = Source::Recordings;
    terminal.draw(|f| render::draw(f, &mut app, &data)).unwrap();
    assert!(text(&terminal).contains(crate::t!("recordings_no_filters")));
    assert_eq!(app.filter_summary.width, 0);
    app.key(key(KeyCode::Char('n')), &data, &tx);
    assert_eq!(app.source, Source::Stations);
    assert!(matches!(rx.recv().unwrap(), Command::Search(_)));
    assert!(matches!(rx.recv().unwrap(), Command::Random(filter) if !filter.recordings));
}

#[test]
fn explicit_station_details_reload_backups_after_a_row_refresh() {
    let mut data = view();
    let mut app = App::default();
    app.inspect_info = true;
    app.info_request = 7;
    data.inspected_request = 7;
    data.inspected = data.status.station.clone();
    data.rows_revision = 2;
    let (tx, rx) = command::channel(10);
    app.request_selected_backups(&data, &tx);
    assert!(
        matches!(rx.recv().unwrap(), Command::Backups(id) if id == data.inspected.as_ref().unwrap().stationuuid)
    );
}

#[test]
fn failed_backup_query_retries_without_a_selection_or_row_change() {
    let dir = tempfile::tempdir().unwrap();
    let config = crate::test_support::config(dir.path());
    let mut engine = Engine::new(config, Arc::new(AtomicBool::new(false))).unwrap();
    let row = station("Retry");
    engine.store.upsert(&row).unwrap();
    engine
        .store
        .add_backup(&row.stationuuid, "https://example.com/recovered")
        .unwrap();
    let shared = Arc::new(Mutex::new(view()));
    let mut filter = Filter {
        limit: 100,
        ..Filter::default()
    };
    let mut job = None;
    worker::refresh_if_needed(&engine, &filter, &shared, false).unwrap();
    let snapshot = shared.lock().unwrap().snapshot();
    let mut app = App::default();
    let (tx, rx) = command::channel(1);
    let now = Instant::now();
    app.request_selected_backups_at(&snapshot, &tx, now);
    let command = rx.try_recv().unwrap();
    let conn = rusqlite::Connection::open(dir.path().join("radio.db")).unwrap();
    conn.execute_batch("ALTER TABLE streams RENAME TO unavailable_streams")
        .unwrap();
    worker::execute(command, &mut engine, &mut filter, &mut job, &shared).unwrap();
    let failed = shared.lock().unwrap().snapshot();
    assert!(failed.backups_errors.contains_key(&row.stationuuid));
    assert!(!failed.backups.contains_key(&row.stationuuid));
    let mut terminal = Terminal::new(TestBackend::new(160, 48)).unwrap();
    terminal
        .draw(|frame| render::draw(frame, &mut app, &failed))
        .unwrap();
    assert!(text(&terminal).contains("Backup URL query failed"));
    assert_no_cjk(&text(&terminal));
    for line in failed.logs.iter() {
        assert_no_cjk(line);
    }
    for seconds in [2, 6, 14, 30, 60, 90] {
        app.request_selected_backups_at(&failed, &tx, now + Duration::from_secs(seconds - 1));
        assert!(rx.try_recv().is_err(), "retry must respect backoff");
        app.request_selected_backups_at(&failed, &tx, now + Duration::from_secs(seconds));
        worker::execute(
            rx.try_recv().unwrap(),
            &mut engine,
            &mut filter,
            &mut job,
            &shared,
        )
        .unwrap();
        let repeated = shared.lock().unwrap().snapshot();
        assert_eq!(repeated.revision, failed.revision);
        assert_eq!(repeated.immediate_revision, failed.immediate_revision);
        assert_eq!(repeated.logs.len(), failed.logs.len());
    }
    worker::execute(
        Command::Info(row.stationuuid.clone(), 7),
        &mut engine,
        &mut filter,
        &mut job,
        &shared,
    )
    .unwrap();
    assert_eq!(shared.lock().unwrap().inspected_request, 7);
    assert!(
        shared.lock().unwrap().inspected.is_some(),
        "a backup failure must not hide station details"
    );
    app.request_selected_backups_at(&failed, &tx, now + Duration::from_secs(119));
    assert!(rx.try_recv().is_err(), "failure must not flood the queue");
    conn.execute_batch("ALTER TABLE unavailable_streams RENAME TO streams")
        .unwrap();
    app.request_selected_backups_at(&failed, &tx, now + Duration::from_secs(120));
    worker::execute(
        rx.try_recv().unwrap(),
        &mut engine,
        &mut filter,
        &mut job,
        &shared,
    )
    .unwrap();
    let recovered = shared.lock().unwrap().snapshot();
    assert_eq!(recovered.rows_revision, snapshot.rows_revision);
    assert!(recovered.backups_errors.is_empty());
    assert_eq!(
        recovered.backups[&row.stationuuid],
        ["https://example.com/recovered"]
    );
    app.request_selected_backups_at(&recovered, &tx, now + Duration::from_secs(150));
    assert!(rx.try_recv().is_err());
    // A list refresh invalidates both success and failure caches.
    Arc::make_mut(&mut shared.lock().unwrap().backups_errors)
        .insert(row.stationuuid.clone(), "stale failure".into());
    worker::execute(
        Command::Refresh,
        &mut engine,
        &mut filter,
        &mut job,
        &shared,
    )
    .unwrap();
    worker::refresh_if_needed(&engine, &filter, &shared, false).unwrap();
    let refreshed = shared.lock().unwrap().snapshot();
    assert!(refreshed.backups_errors.is_empty());
    assert!(refreshed.backups.is_empty());
}

#[test]
fn facets_query_only_the_latest_field_and_bound_results() {
    let dir = tempfile::tempdir().unwrap();
    let config = crate::test_support::config(dir.path());
    let mut engine = Engine::new(config, Arc::new(AtomicBool::new(false))).unwrap();
    let mut row = station("Facets");
    row.tags = (0..150)
        .map(|i| format!("tag{i:03}"))
        .collect::<Vec<_>>()
        .join(",");
    engine.store.upsert(&row).unwrap();
    let shared = Arc::new(Mutex::new(view()));
    let mut filter = Filter::default();
    let mut job = None;
    for prefix in ["", "t", "tag1"] {
        worker::execute(
            Command::Facets {
                selected: 2,
                prefix: prefix.into(),
            },
            &mut engine,
            &mut filter,
            &mut job,
            &shared,
        )
        .unwrap();
    }
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let data = shared.lock().unwrap();
        if !data.facets_loading {
            assert!(data.facets[0].is_empty() && data.facets[1].is_empty());
            assert_eq!(data.facets[2].len(), 50);
            assert!(
                data.facets[2]
                    .iter()
                    .all(|facet| facet.value.starts_with("tag1"))
            );
            break;
        }
        drop(data);
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(5));
    }
    let (tx, rx) = command::channel(1);
    let data = shared.lock().unwrap().snapshot();
    let mut app = App::default();
    app.key(key(KeyCode::Char('F')), &data, &tx);
    // Queue pressure must leave the latest query available for retry.
    tx.try_send(Command::Refresh).unwrap();
    app.request_facets(&tx);
    rx.try_recv().unwrap();
    app.request_facets(&tx);
    assert!(matches!(
        rx.try_recv(),
        Ok(Command::Facets { selected: 0, .. })
    ));
    app.key(key(KeyCode::Tab), &data, &tx);
    app.request_facets(&tx);
    assert!(matches!(
        rx.try_recv(),
        Ok(Command::Facets { selected: 1, .. })
    ));
}

#[test]
fn jump_shortcut_selects_the_playing_row_without_replaying() {
    let mut data = view();
    Arc::make_mut(&mut data.rows).push(data.status.station.clone().unwrap());
    let (tx, rx) = command::channel(5);
    let mut app = App::default();
    app.focus = Focus::Details;
    app.inspect_info = true;
    app.key(key(KeyCode::Char('\'')), &data, &tx);
    assert_eq!(app.list.selected(), Some(0));
    app.key(key(KeyCode::Char('\'')), &data, &tx);
    assert_eq!(app.focus, Focus::List);
    assert_eq!(app.list.selected(), Some(2));
    assert!(!app.inspect_info);
    assert!(rx.try_recv().is_err());
    app.jump_binding = "<Ctrl-p>".into();
    app.list.select(Some(0));
    app.key(
        KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL),
        &data,
        &tx,
    );
    assert_eq!(app.list.selected(), Some(2));
    app.jump_binding = "<Ctrl-P>".into();
    app.list.select(Some(0));
    app.key(
        KeyEvent::new(
            KeyCode::Char('P'),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        ),
        &data,
        &tx,
    );
    assert_eq!(app.list.selected(), Some(2));
    app.jump_binding.clear();
    app.list.select(Some(0));
    app.key(key(KeyCode::Char('\'')), &data, &tx);
    app.key(key(KeyCode::Char('\'')), &data, &tx);
    assert_eq!(app.list.selected(), Some(0));
    app.jump_binding = "<Ctrl-p>".into();
    app.mode = Mode::Search;
    app.key(
        KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL),
        &data,
        &tx,
    );
    assert_eq!(app.mode, Mode::Search);
    app.mode = Mode::Normal;
    data.status.playback_state = crate::engine::PlaybackState::Idle;
    app.key(
        KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL),
        &data,
        &tx,
    );
    assert!(!app.message.is_empty());
    assert!(rx.try_recv().is_err());
}

#[test]
fn jump_locates_a_filtered_out_station_on_another_page() {
    let dir = tempfile::tempdir().unwrap();
    let config = crate::test_support::config(dir.path());
    let mut engine = Engine::new(config, Arc::new(AtomicBool::new(false))).unwrap();
    let mut rows = vec![];
    for i in 0..12 {
        let row = station(&format!("Station {i:02}"));
        engine.store.upsert(&row).unwrap();
        rows.push(row);
    }
    let mut data = view();
    data.status.station = Some(rows[9].clone());
    let shared = Arc::new(Mutex::new(data));
    let mut filter = Filter {
        query: "Station 00".into(),
        sort: crate::model::Sort::Random,
        limit: 4,
        ..Filter::default()
    };
    let mut job = None;
    worker::execute(
        Command::Search(filter.clone()),
        &mut engine,
        &mut filter,
        &mut job,
        &shared,
    )
    .unwrap();
    worker::refresh_if_needed(&engine, &filter, &shared, false).unwrap();
    let mut app = App::default();
    app.filter = filter.clone();
    app.jump_binding = "<F6>".into();
    let (tx, rx) = command::channel(3);
    let snapshot = shared.lock().unwrap().snapshot();
    app.key(key(KeyCode::F(6)), &snapshot, &tx);
    worker::execute(
        rx.try_recv().unwrap(),
        &mut engine,
        &mut filter,
        &mut job,
        &shared,
    )
    .unwrap();
    app.reconcile(&shared.lock().unwrap().snapshot());
    assert_eq!(app.filter.offset, 8);
    assert_eq!(app.filter.sort, crate::model::Sort::Clickcount);
    assert!(app.filter.query.is_empty());
    app.flush_search(&tx);
    worker::execute(
        rx.try_recv().unwrap(),
        &mut engine,
        &mut filter,
        &mut job,
        &shared,
    )
    .unwrap();
    worker::refresh_if_needed(&engine, &filter, &shared, false).unwrap();
    app.reconcile(&shared.lock().unwrap().snapshot());
    assert_eq!(app.list.selected(), Some(1));
    assert_eq!(
        app.selected(&shared.lock().unwrap()).unwrap().stationuuid,
        rows[9].stationuuid
    );
    assert!(rx.try_recv().is_err());
    // A newer source command supersedes a pending locate response.
    app.source = Source::Recordings;
    app.key(key(KeyCode::F(6)), &snapshot, &tx);
    worker::execute(
        rx.try_recv().unwrap(),
        &mut engine,
        &mut filter,
        &mut job,
        &shared,
    )
    .unwrap();
    app.key(key(KeyCode::Char(']')), &snapshot, &tx);
    let chosen = app.filter.clone();
    app.reconcile(&shared.lock().unwrap().snapshot());
    assert_eq!(app.filter, chosen);
}

#[test]
fn jump_waits_for_a_fresh_page_even_when_the_filter_is_unchanged() {
    let mut data = view();
    let (tx, rx) = command::channel(2);
    let mut app = App::default();
    app.jump_binding = "gp".into();
    app.key(key(KeyCode::Char('g')), &data, &tx);
    app.key(key(KeyCode::Char('p')), &data, &tx);
    let request = match rx.try_recv().unwrap() {
        Command::LocatePlaying { request, .. } => request,
        _ => panic!("expected locate"),
    };
    data.located = Some(worker::LocatedStation {
        request,
        filter: Ok(app.filter.clone()),
    });
    app.reconcile(&data);
    app.flush_search(&tx);
    rx.try_recv().unwrap();
    app.reconcile(&data); // A slow refresh must not replace the target UUID with row zero.
    Arc::make_mut(&mut data.rows).push(data.status.station.clone().unwrap());
    data.rows_revision += 1;
    app.reconcile(&data);
    assert_eq!(app.list.selected(), Some(2));
    assert!(app.message.is_empty());
    app.list.select(Some(0));
    app.key(key(KeyCode::Char('g')), &data, &tx);
    app.key(key(KeyCode::Esc), &data, &tx);
    app.key(key(KeyCode::Char('p')), &data, &tx);
    assert_eq!(
        app.list.selected(),
        Some(0),
        "Esc cancels a partial sequence"
    );
}

#[test]
fn jump_locates_the_last_requested_station_after_a_failure_without_replaying() {
    let failed = || crate::engine::PlaybackState::Failed {
        reason: "stream refused".into(),
    };
    let (tx, rx) = command::channel(4);

    // The station is already on the current page.
    let mut data = view();
    Arc::make_mut(&mut data.rows).push(data.status.station.clone().unwrap());
    data.status.playback_state = failed();
    let mut app = App::default();
    app.jump_binding = "<F6>".into();
    app.focus = Focus::Details;
    app.key(key(KeyCode::F(6)), &data, &tx);
    assert_eq!(app.list.selected(), Some(2));
    assert_eq!(app.focus, Focus::List);
    assert_eq!(app.message, crate::t!("jump_failed"));
    assert!(rx.try_recv().is_err(), "locating must never replay");

    // The station sits on another page, so the locate round-trips.
    let mut data = view();
    data.status.playback_state = failed();
    let mut app = App::default();
    app.jump_binding = "<F6>".into();
    app.key(key(KeyCode::F(6)), &data, &tx);
    assert_eq!(app.message, crate::t!("jump_loading"));
    let request = match rx.try_recv().unwrap() {
        Command::LocatePlaying { request, .. } => request,
        _ => panic!("expected locate"),
    };
    data.located = Some(worker::LocatedStation {
        request,
        filter: Ok(app.filter.clone()),
    });
    app.reconcile(&data);
    app.flush_search(&tx);
    assert!(matches!(rx.try_recv(), Ok(Command::Search(_))));
    Arc::make_mut(&mut data.rows).push(data.status.station.clone().unwrap());
    data.rows_revision += 1;
    app.reconcile(&data);
    assert_eq!(app.list.selected(), Some(2));
    assert_eq!(app.message, crate::t!("jump_failed"));
    assert!(rx.try_recv().is_err(), "locating must never replay");

    // A recovered playback shows no failure notice, and an idle player still refuses.
    data.status.playback_state = crate::engine::PlaybackState::Playing;
    app.list.select(Some(0));
    app.key(key(KeyCode::F(6)), &data, &tx);
    assert_eq!(app.list.selected(), Some(2));
    assert!(app.message.is_empty());
    data.status.playback_state = crate::engine::PlaybackState::Idle;
    app.list.select(Some(0));
    app.key(key(KeyCode::F(6)), &data, &tx);
    assert_eq!(app.list.selected(), Some(0));
    assert_eq!(app.message, crate::t!("jump_not_playing"));
    assert!(rx.try_recv().is_err());
}

#[test]
fn small_terminal_blocks_actions_and_defers_search_until_visible() {
    use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
    let data = view();
    let (sender, receiver) = command::channel(16);
    for (width, height) in [(39, 16), (40, 15), (0, 0)] {
        let mut app = App::default();
        app.key(key(KeyCode::Char(':')), &data, &sender);
        app.editor.set("station remove frozen-id");
        app.key(key(KeyCode::Enter), &data, &sender);
        assert!(app.confirmation.is_some());
        app.message = "Confirm removal of frozen-id".into();
        app.update_size(width, height);
        assert!(app.confirmation.is_none());
        assert!(app.message.is_empty());
        app.search_due = Some(Instant::now());
        for code in [
            KeyCode::Enter,
            KeyCode::Char('n'),
            KeyCode::Char('r'),
            KeyCode::Char('D'),
            KeyCode::Char('y'),
            KeyCode::Char(':'),
        ] {
            app.key(key(code), &data, &sender);
        }
        app.mouse(
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 1,
                row: 1,
                modifiers: KeyModifiers::NONE,
            },
            &data,
            &sender,
        );
        app.flush_search(&sender);
        assert!(receiver.try_recv().is_err());
        assert!(app.search_due.is_some());
        app.update_size(80, 24);
        app.flush_search(&sender);
        assert!(matches!(receiver.try_recv(), Ok(Command::Search(_))));
        assert!(app.search_due.is_none());
    }
    let mut app = App::default();
    app.key(key(KeyCode::Char(':')), &data, &sender);
    app.editor.set("play draft");
    app.update_size(39, 16);
    app.key(key(KeyCode::Enter), &data, &sender);
    app.update_size(80, 24);
    assert_eq!(app.editor.text, "play draft");
    assert_eq!(app.mode, Mode::Command);
    assert!(receiver.try_recv().is_err());
}

#[test]
fn long_notices_use_both_bottom_rows_without_losing_the_reason() {
    use crate::messages::{Locale, text_in};
    let data = view();
    let compact = |text: &str| {
        text.chars()
            .filter(|c| !c.is_whitespace())
            .collect::<String>()
    };
    for locale in [Locale::En, Locale::ZhCn] {
        let message = format!(
            "{} · {}",
            text_in(locale, "command_submitted"),
            text_in(locale, "history_withheld")
        );
        for (width, height) in [(40, 16), (60, 20), (80, 24), (100, 24)] {
            let mut app = App::default();
            app.message = message.clone();
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal.draw(|f| render::draw(f, &mut app, &data)).unwrap();
            let rendered = text(&terminal);
            let rows: Vec<_> = rendered.lines().collect();
            let notice = compact(&rows[usize::from(height) - 2..].concat());
            assert!(
                notice.contains(&compact(&message)),
                "{locale:?}, {width}: {rendered}"
            );
            assert!(rows[usize::from(height) - 3].contains(crate::t!("mode_normal")));
            assert_eq!(app.message, message);
            assert_eq!(app.filter_summary.y, height - 3);
            // A short notice leaves the usual keyboard hints visible.
            app.message = crate::t!("command_submitted").into();
            terminal.draw(|f| render::draw(f, &mut app, &data)).unwrap();
            let keys = if width < 80 {
                crate::t!("keys_compact")
            } else {
                crate::t!("keys_global")
            }
            .replace('`', "");
            let rendered = text(&terminal);
            assert!(rendered.lines().last().unwrap().starts_with(&keys));
            // Dismissing the notice restores the ordinary context row.
            app.message.clear();
            terminal.draw(|f| render::draw(f, &mut app, &data)).unwrap();
            assert!(!compact(&text(&terminal)).contains(&compact(&message)));
        }
    }
}

#[test]
fn bottom_bar_names_the_open_modal_instead_of_the_panel_behind_it() {
    let data = view();
    let (sender, _receiver) = command::channel(4);
    let draw_text = |app: &mut App| {
        let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
        terminal.draw(|f| render::draw(f, app, &data)).unwrap();
        text(&terminal)
    };
    let mut app = App::default();
    let normal = draw_text(&mut app);
    assert!(normal.contains(&format!(" {} · ", crate::t!("mode_normal"))));
    assert!(normal.contains(&crate::t!("keys_global").replace('`', "")));
    app.key(key(KeyCode::Char('?')), &data, &sender);
    let help = draw_text(&mut app);
    assert!(help.contains(&format!(" {} ", crate::t!("mode_help"))));
    assert!(help.contains(crate::t!("help_keys")));
    assert!(!help.contains(&format!(" {} · ", crate::t!("mode_normal"))));
    assert!(!help.contains(&crate::t!("keys_global").replace('`', "")));
    app.key(key(KeyCode::Esc), &data, &sender);
    app.key(key(KeyCode::Char('F')), &data, &sender);
    let filters = draw_text(&mut app);
    assert!(filters.contains(&format!(" {} ", crate::t!("mode_filters"))));
    assert!(!filters.contains(&format!(" {} · ", crate::t!("mode_normal"))));
    app.key(key(KeyCode::Esc), &data, &sender);
    app.key(key(KeyCode::Char(':')), &data, &sender);
    let id = data.rows[0].stationuuid.clone();
    app.editor.set(&format!("station remove {id}"));
    app.key(key(KeyCode::Enter), &data, &sender);
    let confirm = draw_text(&mut app);
    assert!(confirm.contains(&format!(" {} ", crate::t!("mode_confirm"))));
    assert!(!confirm.contains(&format!(" {} · ", crate::t!("mode_normal"))));
    app.key(key(KeyCode::Esc), &data, &sender);
    assert!(draw_text(&mut app).contains(&format!(" {} · ", crate::t!("mode_normal"))));
}

#[test]
fn deletion_confirmation_keeps_target_and_survives_queue_rejection() {
    let mut data = view();
    let (sender, receiver) = command::channel(1);
    let mut app = App::default();
    app.key(key(KeyCode::Char(':')), &data, &sender);
    let id = data.rows[0].stationuuid.clone();
    app.editor.set(&format!("station remove {id}"));
    app.key(key(KeyCode::Enter), &data, &sender);
    app.message = "unrelated background message".into();
    let mut terminal = Terminal::new(TestBackend::new(40, 16)).unwrap();
    terminal.draw(|f| render::draw(f, &mut app, &data)).unwrap();
    let rendered = text(&terminal);
    assert!(rendered.contains(&id));
    assert!(rendered.contains(crate::t!("confirm_delete")));
    let mut repeat = key(KeyCode::Char('y'));
    repeat.kind = crossterm::event::KeyEventKind::Repeat;
    app.key(repeat, &data, &sender);
    assert!(app.confirmation.is_some());
    sender.send(Command::Random(Filter::default())).unwrap();
    app.key(key(KeyCode::Char('y')), &data, &sender);
    assert!(
        app.confirmation
            .as_ref()
            .is_some_and(|c| !c.error.is_empty())
    );
    receiver.try_recv().unwrap();
    data.rows = vec![station("replacement")].into();
    app.key(key(KeyCode::Char('y')), &data, &sender);
    assert!(
        matches!(receiver.try_recv(), Ok(Command::Station(crate::cli::StationAction::Remove { uuid, .. })) if uuid == id)
    );
    assert!(app.confirmation.is_none());
    for cancel in [
        KeyCode::Enter,
        KeyCode::Char('q'),
        KeyCode::Char('n'),
        KeyCode::Esc,
    ] {
        app.key(key(KeyCode::Char(':')), &data, &sender);
        app.editor.set("station remove frozen");
        app.key(key(KeyCode::Enter), &data, &sender);
        for blocked in [KeyCode::Char(' '), KeyCode::Char('r'), KeyCode::Char('m')] {
            app.key(key(blocked), &data, &sender);
        }
        app.key(key(cancel), &data, &sender);
        assert!(app.confirmation.is_none());
        assert!(!app.quit);
        assert!(receiver.try_recv().is_err());
    }
}

#[test]
fn page_failure_disables_stale_objects_and_retries_the_current_filter() {
    use super::worker::{PagePhase, PageState};
    for source in Source::ALL {
        let mut data = view();
        let mut app = App::default();
        app.source = source;
        app.filter.recordings = source == Source::Recordings;
        app.filter.favorites = source == Source::Favorites;
        app.filter.history = source == Source::History;
        app.filter.query = "keep my query".into();
        data.filter = app.filter.clone();
        let (sender, receiver) = command::channel(10);
        data.page = Some(PageState {
            generation: 7,
            requested: app.filter.clone(),
            phase: PagePhase::Error("broken storage".into()),
        });
        assert!(app.selected(&data).is_none());
        assert_eq!(app.length(&data), 0);
        for code in [
            KeyCode::Char('y'),
            KeyCode::Char('Y'),
            KeyCode::Char('D'),
            KeyCode::Char('f'),
        ] {
            app.key(key(code), &data, &sender);
        }
        assert!(app.confirmation.is_none());
        assert!(app.clipboard.is_none());
        assert!(receiver.try_recv().is_err());
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|f| render::draw(f, &mut app, &data)).unwrap();
        assert!(text(&terminal).contains("F5"));
        app.key(key(KeyCode::Enter), &data, &sender);
        assert_eq!(app.help_content.as_deref(), Some("broken storage"));
        app.key(key(KeyCode::Esc), &data, &sender);
        app.key(key(KeyCode::F(5)), &data, &sender);
        let filter = match receiver.try_recv().unwrap() {
            Command::Search(f) | Command::Recordings(f) => f,
            _ => panic!("expected retry"),
        };
        assert_eq!(filter, app.filter);
        data.page.as_mut().unwrap().phase = PagePhase::Ready;
        assert!(data.page_ready(&filter));
    }
}

#[test]
fn corrected_page_is_adopted_only_for_the_current_request() {
    use super::worker::{PagePhase, PageState};
    let mut data = view();
    let mut app = App::default();
    app.filter.offset = 200;
    data.page = Some(PageState {
        generation: 4,
        requested: app.filter.clone(),
        phase: PagePhase::Ready,
    });
    data.filter.offset = 100;
    app.reconcile(&data);
    assert_eq!(app.filter.offset, 100);
    app.filter.query = "new search".into();
    app.reconcile(&data);
    assert_eq!(app.filter.query, "new search");
}

#[test]
fn unqueried_page_shows_loading_and_disables_row_actions() {
    let mut data = view();
    data.page = None;
    let mut app = App::default();
    let (sender, receiver) = command::channel(10);
    assert!(!data.page_ready(&app.filter));
    assert!(app.selected(&data).is_none());
    for code in [KeyCode::Enter, KeyCode::Char('f'), KeyCode::Char('y')] {
        app.key(key(code), &data, &sender);
    }
    assert!(receiver.try_recv().is_err());
    assert!(app.clipboard.is_none());
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    terminal.draw(|f| render::draw(f, &mut app, &data)).unwrap();
    assert!(text(&terminal).contains(crate::t!("page_loading")));
    assert!(!text(&terminal).contains("Selected Station"));
    assert!(!text(&terminal).contains(crate::t!("empty_stations")));
}

#[test]
fn minimum_layout_keeps_list_rows_names_and_task_navigation() {
    let mut data = view();
    data.task_active = true;
    data.task = "syncing".into();
    for (width, height) in [(40, 16), (99, 19), (100, 19), (99, 20), (100, 20)] {
        let mut app = App::default();
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|f| render::draw(f, &mut app, &data)).unwrap();
        assert!(app.rects[Focus::List.index()].height >= 5);
        assert!(text(&terminal).contains("Current Station"));
        assert!(text(&terminal).contains("Selected Station"));
        assert!(text(&terminal).contains("q"));
        if height < 20 {
            assert_eq!(app.rects[Focus::Activity.index()].height, 1);
            assert!(text(&terminal).contains("[2/3]"));
            app.focus = Focus::Activity;
            terminal.draw(|f| render::draw(f, &mut app, &data)).unwrap();
            assert!(app.rects[Focus::Activity.index()].height >= 5);
            assert_eq!(
                app.rects[Focus::List.index()],
                ratatui::layout::Rect::default()
            );
            assert_eq!(
                app.rects[Focus::Details.index()],
                ratatui::layout::Rect::default()
            );
            app.focus = Focus::List;
            terminal.draw(|f| render::draw(f, &mut app, &data)).unwrap();
            assert!(app.rects[Focus::List.index()].height >= 5);
        }
    }
}

#[test]
fn mouse_hits_only_clear_label_and_modal_wheel_stays_in_modal() {
    use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
    let mut data = view();
    let mut app = App::default();
    app.filter.query = "keep".into();
    data.filter = app.filter.clone();
    let (sender, receiver) = command::channel(16);
    let mut terminal = Terminal::new(TestBackend::new(40, 16)).unwrap();
    terminal.draw(|f| render::draw(f, &mut app, &data)).unwrap();
    let clear = app.filter_summary;
    let event = |kind, column, row| MouseEvent {
        kind,
        column,
        row,
        modifiers: KeyModifiers::NONE,
    };
    app.mouse(
        event(MouseEventKind::Down(MouseButton::Left), 0, clear.y),
        &data,
        &sender,
    );
    assert_eq!(app.filter.query, "keep");
    assert!(receiver.try_recv().is_err());
    app.mouse(
        event(MouseEventKind::Down(MouseButton::Left), clear.x, clear.y),
        &data,
        &sender,
    );
    assert!(matches!(receiver.try_recv(), Ok(Command::Search(filter)) if filter.query.is_empty()));
    app.help = true;
    app.mouse(
        event(MouseEventKind::ScrollDown, clear.x, clear.y),
        &data,
        &sender,
    );
    assert_eq!(app.help_scroll, 1);
    assert!(receiver.try_recv().is_err());
    app.help = false;
    app.filter_edit = Some((0, Default::default()));
    Arc::make_mut(&mut data.facets)[0] = vec![crate::model::Facet {
        value: "CN".into(),
        count: 1,
    }];
    app.mouse(
        event(MouseEventKind::ScrollDown, clear.x, clear.y),
        &data,
        &sender,
    );
    assert_eq!(app.filter_edit.as_ref().unwrap().1[0], "CN");
    assert!(receiver.try_recv().is_err());
    app.filter_edit = None;
    let player = app.rects[Focus::Player.index()];
    let focus = app.focus;
    app.mouse(
        event(MouseEventKind::ScrollDown, player.x, player.y),
        &data,
        &sender,
    );
    assert_eq!(app.focus, focus);
}

#[test]
fn recording_double_click_respects_data_and_layout_changes() {
    use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
    let mut data = view();
    data.recordings = vec![
        serde_json::from_value(serde_json::json!({
            "id":"record-a", "stationuuid":"s", "station_name":"Saved audio",
            "source_url_resolved":"https://example.com", "started_at":"2026-01-01",
            "stopped_at":null, "media_duration_ms":null, "codec":"mp3", "container":"mp3",
            "path":"/tmp/sample", "size_bytes":1, "status":"complete", "error":null
        }))
        .unwrap(),
    ]
    .into();
    let mut app = App::default();
    app.source = Source::Recordings;
    let (sender, receiver) = command::channel(16);
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    terminal.draw(|f| render::draw(f, &mut app, &data)).unwrap();
    let rect = app.rects[Focus::List.index()];
    let click = MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: rect.x + 2,
        row: rect.y + 1,
        modifiers: KeyModifiers::NONE,
    };
    let now = Instant::now();
    app.mouse_at(click, &data, &sender, now);
    data.task_active = true;
    terminal.draw(|f| render::draw(f, &mut app, &data)).unwrap();
    app.mouse_at(click, &data, &sender, now + Duration::from_millis(100));
    assert!(receiver.try_recv().is_err());
    data.recordings_revision += 1;
    app.mouse_at(click, &data, &sender, now + Duration::from_millis(200));
    assert!(receiver.try_recv().is_err());
    app.mouse_at(click, &data, &sender, now + Duration::from_millis(300));
    assert!(matches!(receiver.try_recv(), Ok(Command::PlayRecording(id)) if id == "record-a"));
}

#[test]
fn persistent_operation_error_has_details_and_explicit_dismissal() {
    let mut data = view();
    data.operation_error_id = 1;
    data.operation_error = "delete failed: cannot remove file".into();
    let mut app = App::default();
    app.message = "Deletion submitted".into();
    let (sender, receiver) = command::channel(10);
    let mut terminal = Terminal::new(TestBackend::new(40, 16)).unwrap();
    terminal.draw(|f| render::draw(f, &mut app, &data)).unwrap();
    assert!(text(&terminal).contains("Esc"));
    app.key(key(KeyCode::Char('?')), &data, &sender);
    assert!(
        app.help_content
            .as_deref()
            .unwrap()
            .contains(&data.operation_error)
    );
    assert!(
        app.help_content
            .as_deref()
            .unwrap()
            .contains("Deletion submitted")
    );
    app.key(key(KeyCode::Esc), &data, &sender);
    assert_eq!(app.dismissed_error, 0);
    app.key(key(KeyCode::Esc), &data, &sender);
    assert_eq!(app.dismissed_error, 1);
    assert!(receiver.try_recv().is_err());
}

#[test]
fn source_preference_write_failure_does_not_block_list_queries() {
    let dir = tempfile::tempdir().unwrap();
    let config = crate::test_support::config(dir.path());
    let mut engine = Engine::new(config, Arc::new(AtomicBool::new(false))).unwrap();
    engine.store.upsert(&station("needle")).unwrap();
    let connection = rusqlite::Connection::open(dir.path().join("radio.db")).unwrap();
    connection
        .execute_batch(
            "CREATE TRIGGER deny_source BEFORE INSERT ON meta
        WHEN NEW.key = 'pref_source' BEGIN SELECT RAISE(ABORT, 'source preference denied'); END;",
        )
        .unwrap();
    let shared = Arc::new(Mutex::new(View::new(engine.status.clone())));
    let mut filter = Filter {
        limit: 100,
        ..Filter::default()
    };
    let mut job = None;
    for recordings in [false, true] {
        let requested = Filter {
            recordings,
            query: "needle".into(),
            limit: 100,
            ..Filter::default()
        };
        worker::execute(
            if recordings {
                Command::Recordings(requested.clone())
            } else {
                Command::Search(requested.clone())
            },
            &mut engine,
            &mut filter,
            &mut job,
            &shared,
        )
        .unwrap();
        worker::refresh_if_needed(&engine, &filter, &shared, false).unwrap();
        let data = shared.lock().unwrap();
        assert!(data.page_ready(&requested));
        assert!(data.operation_error.contains("source preference denied"));
        if !recordings {
            assert_eq!(data.rows.len(), 1);
        }
    }
}

#[test]
fn explore_click_selects_exact_cell_without_playing_and_ignores_gutters() {
    use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
    let data = view();
    let (sender, receiver) = command::channel(20);
    let mut app = App::default();
    app.source = Source::Explore;
    app.focus = Focus::Map;
    let mut terminal = Terminal::new(TestBackend::new(140, 40)).unwrap();
    terminal.draw(|f| render::draw(f, &mut app, &data)).unwrap();
    let area = app.map.area;
    assert!(area.width > 0);
    let point = app.map.point(area.x + 2, area.y + 2);
    let expected = app.map.bounds.cell(2, 2, app.map.size.0, app.map.size.1);
    let click = |x, y| MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: x,
        row: y,
        modifiers: KeyModifiers::NONE,
    };
    app.mouse(click(area.x + 2, area.y + 2), &data, &sender);
    assert_eq!(app.map.cursor, point);
    assert_eq!(app.filter.geo, Some(crate::geo::GeoFilter::Cell(expected)));
    assert!(matches!(receiver.try_recv(), Ok(Command::Search(_))));
    assert!(receiver.try_recv().is_err());
    app.mouse(click(area.x - 1, area.y), &data, &sender);
    assert!(receiver.try_recv().is_err());
    for code in [KeyCode::Char('z'), KeyCode::Char('Z'), KeyCode::Char('0')] {
        app.focus = Focus::Map;
        app.key(key(code), &data, &sender);
    }
    assert!(receiver.try_recv().is_err());
}

#[test]
fn explore_narrow_layout_and_country_selection_remain_keyboard_accessible() {
    let mut data = view();
    let (sender, receiver) = command::channel(20);
    let mut app = App::default();
    app.source = Source::Explore;
    app.focus = Focus::Map;
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    terminal.draw(|f| render::draw(f, &mut app, &data)).unwrap();
    assert!(app.map.area.width > 0);
    let request = super::map::Request {
        filter: Filter::default(),
        bounds: crate::geo::GeoBounds::default(),
        columns: 1,
        rows: 1,
    };
    data.map_image = Some(Arc::new(super::map::Image {
        request,
        land: Arc::new(vec![' ']),
        counts: vec![0],
        countries: vec![crate::model::Facet {
            value: String::new(),
            count: 2,
        }],
    }));
    app.key(key(KeyCode::Char('c')), &data, &sender);
    assert!(app.map.chooser);
    app.key(key(KeyCode::Enter), &data, &sender);
    assert_eq!(app.filter.geo, Some(crate::geo::GeoFilter::Missing));
    assert_eq!(app.filter.country.as_deref(), Some(""));
    assert!(matches!(receiver.try_recv(), Ok(Command::Search(_))));
    app.key(key(KeyCode::Esc), &data, &sender);
    assert_eq!(app.focus, Focus::Map);
    assert!(matches!(
        app.filter.geo,
        Some(crate::geo::GeoFilter::Cell(_))
    ));
    assert_eq!(app.filter.country, None);
    app.key(key(KeyCode::Char('2')), &data, &sender);
    assert_eq!(app.focus, Focus::List);
    terminal.draw(|f| render::draw(f, &mut app, &data)).unwrap();
    assert_eq!(app.map.area, ratatui::layout::Rect::default());
}

#[test]
fn explore_layout_snapshots_show_map_and_narrow_focus() {
    for (width, height) in [(80, 24), (160, 48)] {
        let mut app = App::default();
        app.source = Source::Explore;
        app.focus = Focus::Map;
        let mut data = view();
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|f| render::draw(f, &mut app, &data)).unwrap();
        let request = super::map::Request {
            filter: Filter::default(),
            bounds: app.map.bounds,
            columns: app.map.size.0,
            rows: app.map.size.1,
        };
        let mut counts = vec![0; request.columns * request.rows];
        for (lat, lon) in [(48.85, 2.35), (31.2, 121.5), (40.7, -74.0)] {
            let (x, y) = crate::geo::coordinates(Some(lat), Some(lon)).unwrap();
            let (x, y) = request
                .bounds
                .project(x, y, request.columns, request.rows)
                .unwrap();
            counts[y * request.columns + x] += 1;
        }
        app.map.submitted = Some((request.clone(), data.geo_revision));
        data.map_image = Some(Arc::new(super::map::Image {
            land: Arc::new(super::map::raster(
                request.bounds,
                request.columns,
                request.rows,
            )),
            counts,
            countries: vec![],
            request,
        }));
        terminal.draw(|f| render::draw(f, &mut app, &data)).unwrap();
        let rendered = text(&terminal)
            .lines()
            .map(str::trim_end)
            .collect::<Vec<_>>()
            .join("\n")
            + "\n";
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join(format!("tests/snapshots/{width}x{height}-explore.txt"));
        if std::env::var_os("UPDATE_SNAPSHOTS").is_some() {
            std::fs::write(&path, &rendered).unwrap();
        }
        assert_eq!(rendered, std::fs::read_to_string(path).unwrap());
    }
}
