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
        Parsed::Worker(Command::Sync)
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
    let (sender, receiver) = mpsc::sync_channel(16);
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
    let (sender, receiver) = mpsc::sync_channel(16);
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
    let (sender, receiver) = mpsc::sync_channel(16);
    let worker_view = view.clone();
    let worker_stop = stop.clone();
    let handle = thread::spawn(move || worker::run(engine, receiver, worker_view, worker_stop));
    sender.send(Command::Sync).unwrap();
    started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    sender.send(Command::Sync).unwrap();
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
    sender.send(Command::Sync).unwrap();
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
    let (tx, _rx) = mpsc::sync_channel(16);
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
    let mut terminal = Terminal::new(TestBackend::new(20, 10)).unwrap();
    terminal.draw(|f| render::draw(f, &mut app, &data)).unwrap();
    app.key(key(KeyCode::Char('q')), &data, &tx);
    assert!(app.quit);
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
    let (tx, rx) = mpsc::sync_channel(16);
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
    for action in 0..5 {
        let (tx, rx) = mpsc::sync_channel(1);
        tx.send(Command::Pause).unwrap();
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
