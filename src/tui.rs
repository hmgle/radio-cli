use crate::{
    config::Config,
    engine::{Engine, Status},
    model::{Filter, Recording, Station},
};
use anyhow::{Result, bail};
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap},
};
use std::{
    io::{self, IsTerminal},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, SyncSender},
    },
    thread,
    time::{Duration, Instant},
};

#[derive(Clone)]
struct View {
    rows: Vec<Station>,
    recordings: Vec<Recording>,
    status: Status,
    catalog_note: String,
    revision: u64,
}
enum Command {
    Search(Filter),
    Play(String),
    Random(Filter),
    Favorite(String, bool),
    Pause,
    Volume(i32),
    Record,
    Recordings,
}

struct Screen;
impl Screen {
    fn enter() -> Result<Self> {
        enable_raw_mode()?;
        let guard = Self;
        execute!(io::stdout(), EnterAlternateScreen)?;
        Ok(guard)
    }
}
impl Drop for Screen {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen);
    }
}

fn worker(
    mut engine: Engine,
    commands: Receiver<Command>,
    view: Arc<Mutex<View>>,
    stop: Arc<AtomicBool>,
) -> Result<()> {
    if engine.store.count()? == 0 {
        {
            let mut data = view.lock().unwrap();
            data.status.message = "获取热门电台…（可按 q 退出）".into();
            data.revision += 1;
        }
        if let Err(e) = engine.directory.bootstrap(&engine.store) {
            engine.status.message = format!("目录获取失败: {e}");
        }
    }
    let note = match engine.store.meta("last_sync")? {
        Some(time) => match chrono::DateTime::parse_from_rfc3339(&time) {
            Ok(time) if chrono::Utc::now().signed_duration_since(time).num_days() < 7 => {
                format!("目录同步: {time}")
            }
            _ => "目录已过期；运行 radio sync 刷新".into(),
        },
        None => "目录尚未完整同步；运行 radio sync 获取全部电台".into(),
    };
    let mut filter = Filter {
        limit: 100,
        ..Filter::default()
    };
    {
        let mut data = view.lock().unwrap();
        data.rows = engine.store.search(&filter)?;
        data.catalog_note = note;
        data.revision += 1;
    }
    let mut last_status = String::new();
    while !stop.load(Ordering::Relaxed) {
        match commands.recv_timeout(Duration::from_millis(100)) {
            Ok(command) => {
                let result = (|| -> Result<()> {
                    match command {
                        Command::Search(value) => {
                            filter = value;
                            view.lock().unwrap().rows = engine.store.search(&filter)?;
                        }
                        Command::Play(uuid) => engine.play(&uuid)?,
                        Command::Random(filter) => engine.random(filter)?,
                        Command::Favorite(uuid, add) => {
                            engine.store.favorite(&uuid, add)?;
                            view.lock().unwrap().rows = engine.store.search(&filter)?;
                        }
                        Command::Pause => engine.pause()?,
                        Command::Volume(delta) => engine.volume(delta)?,
                        Command::Record => engine.toggle_recording()?,
                        Command::Recordings => {
                            view.lock().unwrap().recordings = engine.store.recordings(100)?
                        }
                    }
                    Ok(())
                })();
                if let Err(e) = result {
                    engine.status.message = format!("错误: {e:#}");
                }
                view.lock().unwrap().revision += 1;
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        if let Err(e) = engine.tick() {
            engine.status.message = format!("错误: {e:#}");
        }
        let state = format!("{:?}", engine.status);
        if state != last_status {
            last_status = state;
            let mut data = view.lock().unwrap();
            data.status = engine.status.clone();
            data.revision += 1;
        }
    }
    engine.shutdown()
}

pub fn run(config: Config, stop: Arc<AtomicBool>) -> Result<()> {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        bail!("TUI 需要交互终端");
    }
    let engine = Engine::new(config, stop.clone())?;
    let view = Arc::new(Mutex::new(View {
        rows: vec![],
        recordings: vec![],
        status: engine.status.clone(),
        catalog_note: "目录加载中…".into(),
        revision: 1,
    }));
    let (sender, receiver) = mpsc::sync_channel(8);
    let worker_view = view.clone();
    let worker_stop = stop.clone();
    let handle = thread::spawn(move || worker(engine, receiver, worker_view, worker_stop));
    let result = (|| -> Result<()> {
        let _screen = Screen::enter()?;
        let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
        terminal.clear()?;
        interface(&mut terminal, &sender, &view, &stop)
    })();
    stop.store(true, Ordering::Relaxed);
    drop(sender);
    let worker_result = handle
        .join()
        .map_err(|_| anyhow::anyhow!("界面工作线程异常退出"))?;
    result?;
    worker_result
}

fn interface(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    sender: &SyncSender<Command>,
    view: &Arc<Mutex<View>>,
    stop: &Arc<AtomicBool>,
) -> Result<()> {
    let mut filter = Filter {
        limit: 100,
        ..Filter::default()
    };
    let mut state = ListState::default();
    state.select(Some(0));
    let mut tab = 0usize;
    let mut editing = false;
    let mut filter_edit: Option<(usize, [String; 4])> = None;
    let mut search_due: Option<Instant> = None;
    let mut dirty = true;
    let mut revision = 0;
    let mut local_message = String::new();
    loop {
        if stop.load(Ordering::Relaxed) {
            break;
        }
        let snapshot = view.lock().unwrap().clone();
        if dirty || snapshot.revision != revision {
            terminal.draw(|frame| {
                let area=frame.area();
                if area.width<40 || area.height<10 {frame.render_widget(Paragraph::new("请将终端调整到至少 40×10；q 退出"),area);return;}
                let areas=Layout::default().direction(Direction::Vertical).constraints([
                    Constraint::Length(4),Constraint::Length(3),Constraint::Min(1),Constraint::Length(3),Constraint::Length(2)]).split(area);
                let station=snapshot.status.station.as_ref().map(|s|crate::clean_text(&s.name)).unwrap_or_else(||"尚未播放".into());
                let recording=if snapshot.status.recording {format!(" ● REC {}s{}",snapshot.status.recording_ms/1000,
                    if snapshot.status.paused {"（播放暂停，录音继续）"} else {""})} else {String::new()};
                let head=format!("{} {}  音量 {}%{}\n{}",if snapshot.status.paused {"Ⅱ"} else {"▶"},station,
                    snapshot.status.volume,recording,snapshot.status.title);
                frame.render_widget(Paragraph::new(head).block(Block::default().title(" radio ").borders(Borders::ALL)),areas[0]);
                let labels=["电台","收藏","最近","录音"];
                let nav=labels.iter().enumerate().map(|(i,s)|if i==tab {format!("[{s}]")}else{s.to_string()}).collect::<Vec<_>>().join("  ");
                frame.render_widget(Paragraph::new(format!("{nav}  页 {} | / {}{}",filter.offset/100+1,filter.query,
                    if editing {"▏"} else {""})).block(Block::default().borders(Borders::ALL)),areas[1]);
                let items:Vec<ListItem>=if tab==3 {snapshot.recordings.iter().map(|r|ListItem::new(format!("{} | {} | {}",
                    crate::clean_text(&r.station_name),r.status,crate::clean_text(&r.path)))).collect()}
                    else {snapshot.rows.iter().map(|s|ListItem::new(format!("{}{} [{}] {}k {}{}",
                        if s.favorite {"★ "}else{"  "},crate::clean_text(&s.name),crate::clean_text(&s.countrycode),s.bitrate,
                        crate::clean_text(&s.language),if s.active {""}else{" [目录已失效]"}))).collect()};
                frame.render_stateful_widget(List::new(items).block(Block::default().borders(Borders::ALL))
                    .highlight_style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)).highlight_symbol("› "),areas[2],&mut state);
                let message=if local_message.is_empty() {crate::clean_text(&snapshot.status.message)}else{local_message.clone()};
                frame.render_widget(Paragraph::new(format!("{message}\n{}",snapshot.catalog_note)).wrap(Wrap {trim:true}),areas[3]);
                frame.render_widget(Paragraph::new("Enter 播放  Space 暂停  n 随机  f 收藏  r 录音  +/- 音量\n/ 搜索  F 筛选  Tab 分类  PgUp/PgDn 翻页  q 退出"),areas[4]);
                if let Some((selected,values))=&filter_edit {
                    let popup=ratatui::layout::Rect::new(area.x+2,area.y+2,area.width.saturating_sub(4),8.min(area.height.saturating_sub(4)));
                    frame.render_widget(ratatui::widgets::Clear,popup);
                    let names=["国家代码","语言","标签","最低码率"];
                    let text=names.iter().enumerate().map(|(i,name)|format!("{} {name}: {}",if i==*selected {">"}else{" "},values[i])).collect::<Vec<_>>().join("\n");
                    frame.render_widget(Paragraph::new(format!("{text}\nTab 下一项，Enter 应用，Esc 取消"))
                        .block(Block::default().title("筛选").borders(Borders::ALL)),popup);
                }
            })?;
            revision = snapshot.revision;
            dirty = false;
        }
        if search_due.is_some_and(|due| Instant::now() >= due) {
            send(sender, Command::Search(filter.clone()), &mut local_message);
            search_due = None;
            dirty = true;
        }
        if !event::poll(Duration::from_millis(100))? {
            continue;
        }
        match event::read()? {
            Event::Resize(_, _) => dirty = true,
            Event::Key(key) if key.kind != KeyEventKind::Release => {
                dirty = true;
                local_message.clear();
                if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
                    break;
                }
                if let Some((selected, values)) = &mut filter_edit {
                    match key.code {
                        KeyCode::Esc => filter_edit = None,
                        KeyCode::Tab => *selected = (*selected + 1) % 4,
                        KeyCode::BackTab => *selected = (*selected + 3) % 4,
                        KeyCode::Char(c) => {
                            if values[*selected].len() < 200 {
                                values[*selected].push(c);
                            }
                        }
                        KeyCode::Backspace => {
                            values[*selected].pop();
                        }
                        KeyCode::Enter => match values[3].parse::<u32>() {
                            Ok(bitrate) => {
                                filter.country = nonempty(&values[0]);
                                filter.language = nonempty(&values[1]);
                                filter.tag = nonempty(&values[2]);
                                filter.min_bitrate = bitrate;
                                filter.offset = 0;
                                filter_edit = None;
                                state.select(Some(0));
                                send(sender, Command::Search(filter.clone()), &mut local_message);
                            }
                            Err(_) => local_message = "最低码率必须为非负整数".into(),
                        },
                        _ => {}
                    }
                    continue;
                }
                if editing {
                    match key.code {
                        KeyCode::Esc | KeyCode::Enter => editing = false,
                        KeyCode::Backspace => {
                            filter.query.pop();
                            search_due = Some(Instant::now() + Duration::from_millis(200));
                        }
                        KeyCode::Char(c) if filter.query.len() < 500 => {
                            filter.query.push(c);
                            search_due = Some(Instant::now() + Duration::from_millis(200));
                        }
                        _ => {}
                    }
                    filter.offset = 0;
                    state.select(Some(0));
                    continue;
                }
                let length = if tab == 3 {
                    snapshot.recordings.len()
                } else {
                    snapshot.rows.len()
                };
                let selected = state.selected().unwrap_or(0);
                match key.code {
                    KeyCode::Char('q') => break,
                    KeyCode::Down | KeyCode::Char('j') => {
                        state.select(Some((selected + 1).min(length.saturating_sub(1))))
                    }
                    KeyCode::Up | KeyCode::Char('k') => {
                        state.select(Some(selected.saturating_sub(1)))
                    }
                    KeyCode::Char('/') if tab != 3 => editing = true,
                    KeyCode::Char('F') if tab != 3 => {
                        filter_edit = Some((
                            0,
                            [
                                filter.country.clone().unwrap_or_default(),
                                filter.language.clone().unwrap_or_default(),
                                filter.tag.clone().unwrap_or_default(),
                                filter.min_bitrate.to_string(),
                            ],
                        ))
                    }
                    KeyCode::Tab => {
                        tab = (tab + 1) % 4;
                        filter.favorites = tab == 1;
                        filter.history = tab == 2;
                        filter.offset = 0;
                        state.select(Some(0));
                        send(
                            sender,
                            if tab == 3 {
                                Command::Recordings
                            } else {
                                Command::Search(filter.clone())
                            },
                            &mut local_message,
                        );
                    }
                    KeyCode::PageDown if tab != 3 && length == 100 => {
                        filter.offset = filter.offset.saturating_add(100);
                        state.select(Some(0));
                        send(sender, Command::Search(filter.clone()), &mut local_message);
                    }
                    KeyCode::PageUp if tab != 3 => {
                        filter.offset = filter.offset.saturating_sub(100);
                        state.select(Some(0));
                        send(sender, Command::Search(filter.clone()), &mut local_message);
                    }
                    KeyCode::Enter if tab != 3 => {
                        if let Some(s) = snapshot.rows.get(selected) {
                            send(
                                sender,
                                Command::Play(s.stationuuid.clone()),
                                &mut local_message,
                            );
                        }
                    }
                    KeyCode::Char('f') if tab != 3 => {
                        if let Some(s) = snapshot.rows.get(selected) {
                            send(
                                sender,
                                Command::Favorite(s.stationuuid.clone(), !s.favorite),
                                &mut local_message,
                            );
                        }
                    }
                    KeyCode::Char('n') => {
                        send(sender, Command::Random(filter.clone()), &mut local_message)
                    }
                    KeyCode::Char('r') => send(sender, Command::Record, &mut local_message),
                    KeyCode::Char(' ') => send(sender, Command::Pause, &mut local_message),
                    KeyCode::Char('+') | KeyCode::Char('=') => {
                        send(sender, Command::Volume(5), &mut local_message)
                    }
                    KeyCode::Char('-') => send(sender, Command::Volume(-5), &mut local_message),
                    _ => {}
                }
            }
            _ => {}
        }
    }
    Ok(())
}

fn nonempty(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() {
        None
    } else {
        Some(value.into())
    }
}
fn send(sender: &SyncSender<Command>, command: Command, error: &mut String) {
    if sender.try_send(command).is_err() {
        *error = "操作队列已满，请稍后重试".into();
    }
}
