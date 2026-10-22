use super::{
    command::{self, Command, Parsed},
    render,
    state::{App, Editor, Focus, Mode, Source},
    worker::{self, View},
};
use crate::{
    config::Config,
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
        let details = render::station_details(&row, &data);
        let chinese = crate::territories::display_in(crate::messages::Locale::ZhCn, code, original);
        assert_eq!(chinese, expected);
        let country = crate::territories::display_in(crate::messages::Locale::En, code, original);
        assert!(
            details.contains(&format!("\nCountry: {country}\n")),
            "{code}: {details}"
        );
        assert!(details.contains("\nState/province/area: Catalog subdivision\n"));
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

#[test]
fn help_notice_follows_the_selected_locale() {
    let english = render::help_text(crate::messages::Locale::En, "");
    let chinese = render::help_text(crate::messages::Locale::ZhCn, "");
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
            text.starts_with(crate::messages::text_in(locale, "tui_help")),
            "{locale:?}"
        );
        assert!(!text.contains("Unknown message"), "{locale:?}");
    }
    let with_binding = render::help_text(crate::messages::Locale::En, "''");
    assert!(
        with_binding.contains("Locate the playing station"),
        "{with_binding}"
    );
    assert_no_cjk(&with_binding);
    assert!(with_binding.starts_with(&english), "{with_binding}");
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
    let config = Config::load(None, Some(dir.path()), Some("http://127.0.0.1:1")).unwrap();
    config.prepare().unwrap();
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
        let mut terminal = Terminal::new(TestBackend::new(160, 48)).unwrap();
        terminal
            .draw(|frame| render::draw(frame, &mut app, &data))
            .unwrap();
        assert!(text(&terminal).contains(render::context_hint(source, Focus::List)));
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
    let config = Config::load(None, Some(dir.path()), Some("http://127.0.0.1:1")).unwrap();
    config.prepare().unwrap();
    let mut engine = Engine::new(config, Arc::new(AtomicBool::new(false))).unwrap();
    let data = Arc::new(Mutex::new(View::new(engine.status.clone())));
    let mut filter = Filter {
        limit: 100,
        ..Filter::default()
    };
    let mut job = None;
    for source in Source::ALL {
        let value = Filter {
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
    let config = Config::load(None, Some(dir.path()), Some(&api)).unwrap();
    config.prepare().unwrap();
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
    app.help_scroll = 60;
    terminal
        .draw(|frame| render::draw(frame, &mut app, &data))
        .unwrap();
    assert!(text(&terminal).contains("close help"));
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
    let config = Config::load(None, Some(dir.path()), Some(&api)).unwrap();
    config.prepare().unwrap();
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
            .any(|s| s.contains("正在运行"))
    );
    sender.send(Command::Cancel).unwrap();
    wait_for(|| view.lock().unwrap().task.contains("正在取消"));
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
    let config = Config::load(None, Some(dir.path()), Some("http://127.0.0.1:1")).unwrap();
    config.prepare().unwrap();
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
    let config = Config::load(None, Some(dir.path()), Some("http://127.0.0.1:1")).unwrap();
    config.prepare().unwrap();
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
    assert!(text(&terminal).contains("详情查询失败"));
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
    let config = Config::load(None, Some(dir.path()), Some("http://127.0.0.1:1")).unwrap();
    config.prepare().unwrap();
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
    assert!(error.to_string().contains("仅录制当前播放电台"));
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
    let config = Config::load(None, Some(dir.path()), Some("http://127.0.0.1:1")).unwrap();
    config.prepare().unwrap();
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
    let config = Config::load(None, Some(dir.path()), Some("http://127.0.0.1:1")).unwrap();
    config.prepare().unwrap();
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
    assert!(text(&terminal).contains("备用地址查询失败"));
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
    let config = Config::load(None, Some(dir.path()), Some("http://127.0.0.1:1")).unwrap();
    config.prepare().unwrap();
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
