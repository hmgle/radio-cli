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
        message: "播放中".into(),
        playing: true,
        paused: false,
        volume: 80,
        recording: false,
        recording_ms: 0,
        recording_path: String::new(),
        source_url: "https://example.com/backup".into(),
    });
    data.rows = vec![selected, station("Another Station")];
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

#[test]
fn input_labels_use_display_cells_and_keep_the_cursor_inside() {
    assert_eq!(render::input_geometry("SEARCH /", 20), (11, 9));
    assert_eq!(render::input_geometry("搜索 /", 20), (13, 7));
    assert_eq!(render::input_geometry("e\u{301}", 20), (18, 2));
    assert_eq!(render::input_geometry("搜索 /", 3), (0, 2));
    assert_eq!(render::input_geometry("搜索 /", 0), (0, 0));
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
                assert!(hint.contains("Enter 播放"));
                assert_eq!(hint.contains("f 收藏"), station_actions);
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
        assert_eq!(Source::restore(&engine.store, true).unwrap(), source);
        assert_eq!(
            Source::restore(&engine.store, false).unwrap(),
            Source::Stations
        );
    }
}

#[test]
fn filter_popup_selects_local_candidates_with_the_keyboard() {
    let mut data = view();
    data.facets[0] = vec![
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
    assert!(matches!(receiver.try_recv(), Ok(Command::Facets)));
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
    let handle = thread::spawn(move || worker::run(engine, receiver, worker_view, worker_stop));
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
    editor.remember();
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
    assert!(text(&terminal).contains("当前播放请求地址:"));
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
    assert!(text(&terminal).contains("关闭帮助"));
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
        if width >= 40 && height >= 16 {
            assert!(text(&terminal).contains("码率未知"));
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
    data.rows.swap(0, 1);
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
    let handle = thread::spawn(move || worker::run(engine, receiver, worker_view, worker_stop));
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
    data.rows.resize(100, station("row"));
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
    engine.status.playing = true;
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
    assert!(!engine.status.recording);
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
    let handle = thread::spawn(move || worker::run(engine, receiver, output, worker_stop));
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
    wait(&|v| v.status.playing, Duration::from_secs(3));
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
        &|v| v.status.volume == 37 && v.status.paused,
        Duration::from_millis(250),
    );
    sender
        .send(Command::Record {
            uuid: None,
            duration: None,
            dir: None,
        })
        .unwrap();
    wait(&|v| v.status.recording, Duration::from_secs(1));
    sender.send(Command::RecordStop).unwrap();
    sender.send(Command::Volume(41)).unwrap();
    wait(
        &|v| !v.status.recording && v.status.volume == 41,
        Duration::from_millis(250),
    );
    sender.send(Command::Play(bad.stationuuid.clone())).unwrap();
    wait(
        &|v| v.status.message.contains("后台刷新"),
        Duration::from_secs(2),
    );
    sender.send(Command::Stop).unwrap();
    wait(
        &|v| v.status.message == "已停止播放",
        Duration::from_millis(250),
    );
    sender
        .send(Command::Play(good.stationuuid.clone()))
        .unwrap();
    wait(
        &|v| v.status.playing && v.status.station.as_ref().unwrap().stationuuid == good.stationuuid,
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
    let handle = thread::spawn(move || worker::run(engine, receiver, output, flag));
    sender.send(Command::Play(row.stationuuid)).unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    while !shared.lock().unwrap().status.playing {
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
        data.status.volume != 42 || !data.status.paused
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
    data.recordings = vec![recording("a"), recording("b")];
    data.rows_revision = 1;
    let mut app = App::default();
    app.source = Source::Recordings;
    app.reconcile(&data);
    data.recordings.insert(0, recording("new"));
    data.rows_revision += 1;
    app.reconcile(&data);
    assert_eq!(data.recordings[app.list.selected().unwrap()].id, "a");
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
        data.rows.resize(100, station("row"));
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
            "error":"first line\nFFmpeg disk failure details"
        }))
        .unwrap(),
    ];
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
    let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
    terminal
        .draw(|frame| render::draw(frame, &mut app, &data))
        .unwrap();
    assert!(text(&terminal).contains("FFmpeg disk failure details"));
}

#[test]
fn layout_snapshots_cover_standard_sizes_and_activity_expansion() {
    let mut data = view();
    // Keep fixtures independent of generated station identities and clock time.
    data.catalog_note = "目录已就绪".into();
    data.rows[0].stationuuid = "00000000-0000-0000-0000-000000000000".into();
    for (width, height) in [(80, 24), (100, 24), (160, 48)] {
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
