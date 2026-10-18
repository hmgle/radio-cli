use super::{
    command::{self, Command, Parsed},
    worker::View,
};
use crate::model::{Filter, Station};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{layout::Rect, widgets::ListState};
use std::{
    collections::VecDeque,
    sync::mpsc::SyncSender,
    time::{Duration, Instant},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Focus {
    Sources,
    List,
    Details,
    Player,
    Activity,
}
impl Focus {
    pub const ALL: [Self; 5] = [
        Self::Sources,
        Self::List,
        Self::Details,
        Self::Player,
        Self::Activity,
    ];
    pub fn index(self) -> usize {
        Self::ALL.iter().position(|f| *f == self).unwrap()
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Sources => "来源",
            Self::List => "列表",
            Self::Details => "详情",
            Self::Player => "播放器",
            Self::Activity => "任务与消息",
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Source {
    Stations,
    Favorites,
    History,
    Recordings,
}
impl Source {
    pub const ALL: [Self; 4] = [
        Self::Stations,
        Self::Favorites,
        Self::History,
        Self::Recordings,
    ];
    pub fn index(self) -> usize {
        Self::ALL.iter().position(|s| *s == self).unwrap()
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Stations => "全部电台",
            Self::Favorites => "收藏电台",
            Self::History => "最近播放",
            Self::Recordings => "本地录音",
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Mode {
    Normal,
    Search,
    Command,
}

#[derive(Default)]
pub(super) struct Editor {
    pub text: String,
    /// Byte offset, always at a UTF-8 boundary.
    pub cursor: usize,
    history: VecDeque<String>,
    history_index: Option<usize>,
    draft: String,
    completion: Option<(Vec<String>, usize)>,
}
impl Editor {
    pub fn set(&mut self, text: &str) {
        self.text = text.into();
        self.cursor = self.text.len();
        self.completion = None;
    }
    pub fn begin(&mut self, text: &str) {
        self.set(text);
        self.history_index = None;
        self.draft.clear();
    }
    pub fn remember(&mut self) {
        if !self.text.trim().is_empty() && self.history.back() != Some(&self.text) {
            self.history.push_back(self.text.clone());
        }
        while self.history.len() > 100 {
            self.history.pop_front();
        }
    }
    pub fn key(&mut self, key: KeyEvent, command: bool) {
        let previous = |text: &str, cursor: usize| {
            text[..cursor]
                .char_indices()
                .next_back()
                .map(|(i, _)| i)
                .unwrap_or(0)
        };
        if command && matches!(key.code, KeyCode::Tab | KeyCode::BackTab) {
            self.complete(key.code == KeyCode::BackTab);
            return;
        }
        self.completion = None;
        match key.code {
            KeyCode::Left => self.cursor = previous(&self.text, self.cursor),
            KeyCode::Right => {
                if let Some(c) = self.text[self.cursor..].chars().next() {
                    self.cursor += c.len_utf8();
                }
            }
            KeyCode::Home => self.cursor = 0,
            KeyCode::End => self.cursor = self.text.len(),
            KeyCode::Char('a') if key.modifiers.contains(KeyModifiers::CONTROL) => self.cursor = 0,
            KeyCode::Char('e') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.cursor = self.text.len()
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => self.set(""),
            KeyCode::Char('w') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                let prefix = &self.text[..self.cursor];
                let end = prefix
                    .trim_end()
                    .char_indices()
                    .rev()
                    .find(|(_, c)| c.is_whitespace())
                    .map(|(i, c)| i + c.len_utf8())
                    .unwrap_or(0);
                self.text.drain(end..self.cursor);
                self.cursor = end;
            }
            KeyCode::Backspace if self.cursor > 0 => {
                let start = previous(&self.text, self.cursor);
                self.text.drain(start..self.cursor);
                self.cursor = start;
            }
            KeyCode::Delete => {
                if let Some(c) = self.text[self.cursor..].chars().next() {
                    self.text.drain(self.cursor..self.cursor + c.len_utf8());
                }
            }
            KeyCode::Up if command => {
                if self.history.is_empty() {
                    return;
                }
                let index = match self.history_index {
                    Some(i) => i.saturating_sub(1),
                    None => {
                        self.draft = self.text.clone();
                        self.history.len() - 1
                    }
                };
                self.history_index = Some(index);
                let value = self.history[index].clone();
                self.set(&value);
            }
            KeyCode::Down if command => {
                if let Some(index) = self.history_index {
                    let value = if index + 1 < self.history.len() {
                        self.history_index = Some(index + 1);
                        self.history[index + 1].clone()
                    } else {
                        self.history_index = None;
                        self.draft.clone()
                    };
                    self.set(&value);
                }
            }
            KeyCode::Char(c)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
                    && !c.is_control()
                    && self.text.len() < 2048 =>
            {
                self.text.insert(self.cursor, c);
                self.cursor += c.len_utf8();
            }
            _ => {}
        }
    }
    fn complete(&mut self, reverse: bool) {
        if let Some((choices, index)) = &mut self.completion {
            *index = if reverse {
                (*index + choices.len() - 1) % choices.len()
            } else {
                (*index + 1) % choices.len()
            };
            self.text = choices[*index].clone();
            self.cursor = self.text.len();
            return;
        }
        if self.cursor != self.text.len() {
            return;
        }
        let mut choices: Vec<String> = command::COMPLETIONS
            .iter()
            .filter(|s| s.starts_with(&self.text))
            .map(|s| s.to_string())
            .collect();
        if choices.is_empty() {
            let start = self.text.rfind(' ').map(|i| i + 1).unwrap_or(0);
            let prefix = &self.text[start..];
            for option in [
                "--country",
                "--language",
                "--tag",
                "--min-bitrate",
                "--limit",
                "--offset",
                "--duration",
                "--output-dir",
                "--backup-url",
            ] {
                if prefix.starts_with('-') && option.starts_with(prefix) {
                    choices.push(format!("{}{option}", &self.text[..start]));
                }
            }
        }
        if choices.is_empty() {
            return;
        }
        let index = if reverse { choices.len() - 1 } else { 0 };
        self.text = choices[index].clone();
        self.cursor = self.text.len();
        self.completion = Some((choices, index));
    }
}

pub(super) struct App {
    pub focus: Focus,
    pub source: Source,
    pub mode: Mode,
    pub filter: Filter,
    pub list: ListState,
    pub editor: Editor,
    pub message: String,
    pub search_due: Option<Instant>,
    original_query: String,
    pub filter_edit: Option<(usize, [String; 4])>,
    pub help: bool,
    pub help_scroll: u16,
    pub details_scroll: u16,
    pub inspect_info: bool,
    pub activity_scroll: u16,
    pub rects: [Rect; 5],
    pub source_rows: [Rect; 4],
    positions: [usize; 4],
    pub quit: bool,
    pub too_small: bool,
    pending_g: bool,
    rows_revision: u64,
    selected_id: Option<String>,
}
impl Default for App {
    fn default() -> Self {
        let mut list = ListState::default();
        list.select(Some(0));
        Self {
            focus: Focus::List,
            source: Source::Stations,
            mode: Mode::Normal,
            filter: Filter {
                limit: 100,
                ..Filter::default()
            },
            list,
            editor: Editor::default(),
            message: String::new(),
            search_due: None,
            original_query: String::new(),
            filter_edit: None,
            help: false,
            help_scroll: 0,
            details_scroll: 0,
            inspect_info: false,
            activity_scroll: 0,
            rects: [Rect::default(); 5],
            source_rows: [Rect::default(); 4],
            positions: [0; 4],
            quit: false,
            too_small: false,
            pending_g: false,
            rows_revision: 0,
            selected_id: None,
        }
    }
}
impl App {
    pub fn selected<'a>(&self, view: &'a View) -> Option<&'a Station> {
        if self.source == Source::Recordings || self.filter != view.filter {
            None
        } else {
            view.rows.get(self.list.selected().unwrap_or(0))
        }
    }
    pub fn length(&self, view: &View) -> usize {
        if self.source == Source::Recordings {
            view.recordings.len()
        } else if self.filter != view.filter {
            0
        } else {
            view.rows.len()
        }
    }
    pub fn reconcile(&mut self, view: &View) {
        let len = self.length(view);
        if self.source != Source::Recordings && self.filter != view.filter {
            return;
        }
        if view.rows_revision != self.rows_revision {
            if self.source != Source::Recordings
                && let Some(id) = &self.selected_id
                && let Some(i) = view.rows.iter().position(|s| &s.stationuuid == id)
            {
                self.list.select(Some(i));
            }
            self.rows_revision = view.rows_revision;
        }
        self.list.select(if len == 0 {
            None
        } else {
            Some(self.list.selected().unwrap_or(0).min(len - 1))
        });
        self.selected_id = self.selected(view).map(|s| s.stationuuid.clone());
    }
    fn send(&mut self, sender: &SyncSender<Command>, command: Command) -> bool {
        match sender.try_send(command) {
            Ok(()) => true,
            Err(error) => {
                self.message = format!("无法提交操作，请稍后重试: {error}");
                false
            }
        }
    }
    pub fn flush_search(&mut self, sender: &SyncSender<Command>) {
        if self.send(sender, Command::Search(self.filter.clone())) {
            self.search_due = None;
        } else {
            self.search_due = Some(Instant::now() + Duration::from_millis(200));
        }
    }
    fn reset_selection(&mut self) {
        self.list = ListState::default().with_selected(Some(0));
        self.selected_id = None;
        self.details_scroll = 0;
        self.inspect_info = false;
    }
    fn switch_source(&mut self, source: Source, sender: &SyncSender<Command>) {
        self.positions[self.source.index()] = self.list.selected().unwrap_or(0);
        self.source = source;
        self.list = ListState::default().with_selected(Some(self.positions[source.index()]));
        self.selected_id = None;
        self.details_scroll = 0;
        self.inspect_info = false;
        self.filter.favorites = source == Source::Favorites;
        self.filter.history = source == Source::History;
        self.filter.offset = 0;
        self.search_due = None;
        self.send(
            sender,
            if source == Source::Recordings {
                Command::Recordings
            } else {
                Command::Search(self.filter.clone())
            },
        );
    }
    fn focus_direction(&mut self, dx: i32, dy: i32) {
        let origin = self.rects[self.focus.index()];
        let x = i32::from(origin.x) * 2 + i32::from(origin.width);
        let y = i32::from(origin.y) * 2 + i32::from(origin.height);
        if let Some((_, focus)) = Focus::ALL
            .iter()
            .filter(|f| **f != self.focus)
            .filter_map(|f| {
                let rect = self.rects[f.index()];
                let a = i32::from(rect.x) * 2 + i32::from(rect.width) - x;
                let b = i32::from(rect.y) * 2 + i32::from(rect.height) - y;
                // A wide player/activity panel can have its center to the right
                // of the list, but it is above/below it rather than beside it.
                let forward = match (dx, dy) {
                    (1, 0) => i32::from(rect.x) - i32::from(origin.right()),
                    (-1, 0) => i32::from(origin.x) - i32::from(rect.right()),
                    (0, 1) => i32::from(rect.y) - i32::from(origin.bottom()),
                    _ => i32::from(origin.y) - i32::from(rect.bottom()),
                };
                let gap = if dx != 0 {
                    origin.y.saturating_sub(rect.bottom()) + rect.y.saturating_sub(origin.bottom())
                } else {
                    origin.x.saturating_sub(rect.right()) + rect.x.saturating_sub(origin.right())
                };
                let lateral = (a * dy - b * dx).abs();
                (forward >= 0 && rect.width > 0 && rect.height > 0)
                    .then_some(((gap, forward, lateral), *f))
            })
            .min_by_key(|(distance, _)| *distance)
        {
            self.focus = focus;
        }
    }
    fn move_selection(&mut self, delta: i32, view: &View) {
        match self.focus {
            Focus::Sources => {}
            Focus::List => {
                let index = (self.list.selected().unwrap_or(0) as i64 + i64::from(delta))
                    .clamp(0, self.length(view).saturating_sub(1) as i64)
                    as usize;
                self.list.select(Some(index));
                self.selected_id = self.selected(view).map(|s| s.stationuuid.clone());
                self.details_scroll = 0;
                self.inspect_info = false;
            }
            Focus::Details => {
                self.details_scroll = (i32::from(self.details_scroll) + delta).clamp(0, 4096) as u16
            }
            Focus::Activity => {
                self.activity_scroll =
                    (i32::from(self.activity_scroll) - delta).clamp(0, 199) as u16
            }
            Focus::Player => {}
        }
    }
    fn execute_line(&mut self, view: &View, sender: &SyncSender<Command>) {
        match command::parse(
            &self.editor.text,
            self.selected(view),
            view.status.station.as_ref(),
            &self.filter,
        ) {
            Ok(parsed) => {
                self.editor.remember();
                self.mode = Mode::Normal;
                self.message.clear();
                // A command supersedes any pending incremental search.
                if self.search_due.is_some() {
                    self.flush_search(sender);
                }
                match parsed {
                    Parsed::Quit => self.quit = true,
                    Parsed::Help => {
                        self.help = true;
                        self.help_scroll = 0;
                    }
                    Parsed::Focus(focus) => self.focus = focus,
                    Parsed::Source(source) => {
                        self.switch_source(source, sender);
                        self.focus = Focus::List;
                    }
                    Parsed::Search(mut filter) => {
                        filter.favorites = self.source == Source::Favorites;
                        filter.history = self.source == Source::History;
                        if self.source == Source::Recordings {
                            self.source = Source::Stations;
                        }
                        self.filter = filter;
                        self.reset_selection();
                        self.focus = Focus::List;
                        self.send(sender, Command::Search(self.filter.clone()));
                    }
                    Parsed::Worker(command) => {
                        if matches!(command, Command::Info(_)) {
                            self.focus = Focus::Details;
                            self.details_scroll = 0;
                            self.inspect_info = true;
                        }
                        if matches!(command, Command::Doctor) {
                            self.focus = Focus::Activity;
                        }
                        if self.send(sender, command) {
                            self.message = "命令已提交".into();
                        }
                    }
                }
            }
            Err(error) => self.message = crate::clean_text(&format!("{error}")),
        }
    }
    pub fn key(&mut self, key: KeyEvent, view: &View, sender: &SyncSender<Command>) {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c')
            || self.too_small && key.code == KeyCode::Char('q')
        {
            self.quit = true;
            return;
        }
        if self.mode != Mode::Normal {
            if key.code == KeyCode::Esc {
                if self.mode == Mode::Search {
                    self.filter.query = self.original_query.clone();
                    self.filter.offset = 0;
                    self.search_due = Some(Instant::now());
                }
                self.mode = Mode::Normal;
                self.message.clear();
                return;
            }
            if key.code == KeyCode::Enter {
                if self.mode == Mode::Command {
                    self.execute_line(view, sender);
                } else {
                    self.flush_search(sender);
                    self.mode = Mode::Normal;
                }
                return;
            }
            self.editor.key(key, self.mode == Mode::Command);
            if self.mode == Mode::Search {
                self.filter.query = self.editor.text.clone();
                self.filter.offset = 0;
                self.reset_selection();
                self.search_due = Some(Instant::now() + Duration::from_millis(200));
            }
            self.message.clear();
            return;
        }
        if self.help {
            match key.code {
                KeyCode::Esc | KeyCode::Char('?') | KeyCode::Char('q') => self.help = false,
                KeyCode::Down | KeyCode::Char('j') => {
                    self.help_scroll = self.help_scroll.saturating_add(1).min(60)
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    self.help_scroll = self.help_scroll.saturating_sub(1)
                }
                KeyCode::PageDown => self.help_scroll = self.help_scroll.saturating_add(10).min(60),
                KeyCode::PageUp => self.help_scroll = self.help_scroll.saturating_sub(10),
                _ => {}
            }
            return;
        }
        if let Some((selected, values)) = &mut self.filter_edit {
            match key.code {
                KeyCode::Esc => self.filter_edit = None,
                KeyCode::Tab => *selected = (*selected + 1) % 4,
                KeyCode::BackTab => *selected = (*selected + 3) % 4,
                KeyCode::Backspace => {
                    values[*selected].pop();
                }
                KeyCode::Char(c)
                    if !c.is_control()
                        && !key.modifiers.contains(KeyModifiers::CONTROL)
                        && values[*selected].len() < 200 =>
                {
                    values[*selected].push(c)
                }
                KeyCode::Enter => match values[3].parse::<u32>() {
                    Ok(bitrate) => {
                        self.filter.country = nonempty(&values[0]);
                        self.filter.language = nonempty(&values[1]);
                        self.filter.tag = nonempty(&values[2]);
                        self.filter.min_bitrate = bitrate;
                        self.filter.offset = 0;
                        self.filter_edit = None;
                        self.reset_selection();
                        self.send(sender, Command::Search(self.filter.clone()));
                    }
                    Err(_) => self.message = "最低码率必须为非负整数".into(),
                },
                _ => {}
            }
            return;
        }
        self.message.clear();
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            match key.code {
                KeyCode::Char('c') => self.quit = true,
                KeyCode::Char('h') | KeyCode::Left => self.focus_direction(-1, 0),
                KeyCode::Char('l') | KeyCode::Right => self.focus_direction(1, 0),
                KeyCode::Char('j') | KeyCode::Down => self.focus_direction(0, 1),
                KeyCode::Char('k') | KeyCode::Up => self.focus_direction(0, -1),
                KeyCode::Char('u') => self.move_selection(-10, view),
                KeyCode::Char('d') => self.move_selection(10, view),
                _ => {}
            }
            return;
        }
        let gg = self.pending_g && key.code == KeyCode::Char('g');
        self.pending_g = false;
        match key.code {
            KeyCode::Char(':') => {
                self.mode = Mode::Command;
                self.editor.begin("");
            }
            KeyCode::Char('/') if self.source != Source::Recordings => {
                self.mode = Mode::Search;
                self.original_query = self.filter.query.clone();
                self.editor.begin(&self.filter.query);
                self.focus = Focus::List;
            }
            KeyCode::Char('?') => {
                self.help = true;
                self.help_scroll = 0;
            }
            KeyCode::Char('q') => self.quit = true,
            KeyCode::Tab => self.focus = Focus::ALL[(self.focus.index() + 1) % 5],
            KeyCode::BackTab => self.focus = Focus::ALL[(self.focus.index() + 4) % 5],
            KeyCode::Char(c @ '1'..='5') => self.focus = Focus::ALL[c as usize - '1' as usize],
            KeyCode::Char('[') => {
                self.switch_source(Source::ALL[(self.source.index() + 3) % 4], sender)
            }
            KeyCode::Char(']') => {
                self.switch_source(Source::ALL[(self.source.index() + 1) % 4], sender)
            }
            KeyCode::Up | KeyCode::Char('k') if self.focus == Focus::Sources => {
                self.switch_source(Source::ALL[(self.source.index() + 3) % 4], sender)
            }
            KeyCode::Down | KeyCode::Char('j') if self.focus == Focus::Sources => {
                self.switch_source(Source::ALL[(self.source.index() + 1) % 4], sender)
            }
            KeyCode::Down | KeyCode::Char('j') => self.move_selection(1, view),
            KeyCode::Up | KeyCode::Char('k') => self.move_selection(-1, view),
            KeyCode::Char('h') | KeyCode::Left => self.focus_direction(-1, 0),
            KeyCode::Char('l') | KeyCode::Right => self.focus_direction(1, 0),
            KeyCode::Home => self.move_selection(-10000, view),
            KeyCode::Char('g') if gg => self.move_selection(-10000, view),
            KeyCode::Char('g') => self.pending_g = true,
            KeyCode::End | KeyCode::Char('G') => self.move_selection(10000, view),
            KeyCode::PageDown | KeyCode::PageUp
                if self.focus == Focus::List && self.source != Source::Recordings =>
            {
                if key.code == KeyCode::PageDown && view.rows.len() == self.filter.limit {
                    self.filter.offset = self.filter.offset.saturating_add(self.filter.limit);
                } else if key.code == KeyCode::PageUp {
                    self.filter.offset = self.filter.offset.saturating_sub(self.filter.limit);
                } else {
                    return;
                }
                self.reset_selection();
                self.send(sender, Command::Search(self.filter.clone()));
            }
            KeyCode::PageDown => self.move_selection(10, view),
            KeyCode::PageUp => self.move_selection(-10, view),
            KeyCode::Enter if self.focus == Focus::Sources => self.focus = Focus::List,
            KeyCode::Enter if self.focus == Focus::Player => {
                if let Some(station) = &view.status.station {
                    self.inspect_info = true;
                    self.details_scroll = 0;
                    self.focus = Focus::Details;
                    self.send(sender, Command::Info(station.stationuuid.clone()));
                }
            }
            KeyCode::Enter if self.focus == Focus::List && self.source != Source::Recordings => {
                if let Some(s) = self.selected(view) {
                    self.send(sender, Command::Play(s.stationuuid.clone()));
                }
            }
            KeyCode::Char('f') if self.source != Source::Recordings => {
                if let Some(s) = self.selected(view) {
                    self.send(
                        sender,
                        Command::Favorite(s.stationuuid.clone(), !s.favorite),
                    );
                }
            }
            KeyCode::Char('F') if self.source != Source::Recordings => {
                self.filter_edit = Some((
                    0,
                    [
                        self.filter.country.clone().unwrap_or_default(),
                        self.filter.language.clone().unwrap_or_default(),
                        self.filter.tag.clone().unwrap_or_default(),
                        self.filter.min_bitrate.to_string(),
                    ],
                ))
            }
            KeyCode::Char('n') => {
                self.send(sender, Command::Random(self.filter.clone()));
            }
            KeyCode::Char('r') => {
                self.send(sender, Command::RecordToggle);
            }
            KeyCode::Char(' ') => {
                self.send(sender, Command::Pause);
            }
            KeyCode::Char('+') | KeyCode::Char('=') => {
                self.send(
                    sender,
                    Command::Volume(view.status.volume.saturating_add(5).min(100)),
                );
            }
            KeyCode::Char('-') => {
                self.send(
                    sender,
                    Command::Volume(view.status.volume.saturating_sub(5)),
                );
            }
            _ => {}
        }
    }
    pub fn mouse(&mut self, event: MouseEvent, view: &View, sender: &SyncSender<Command>) {
        if self.mode != Mode::Normal || self.help || self.filter_edit.is_some() {
            return;
        }
        let point = ratatui::layout::Position::new(event.column, event.row);
        let Some(focus) = std::iter::once(self.focus)
            .chain(Focus::ALL)
            .find(|f| self.rects[f.index()].contains(point))
        else {
            return;
        };
        match event.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                self.focus = focus;
                if focus == Focus::Sources {
                    if let Some(source) = Source::ALL
                        .into_iter()
                        .find(|s| self.source_rows[s.index()].contains(point))
                    {
                        self.switch_source(source, sender);
                    }
                } else if focus == Focus::List {
                    let rect = self.rects[focus.index()];
                    if event.row > rect.y && event.row < rect.bottom().saturating_sub(1) {
                        let index = self.list.offset() + usize::from(event.row - rect.y - 1);
                        if index < self.length(view) {
                            self.list.select(Some(index));
                            self.selected_id = self.selected(view).map(|s| s.stationuuid.clone());
                            self.details_scroll = 0;
                            self.inspect_info = false;
                        }
                    }
                }
            }
            MouseEventKind::ScrollDown => {
                self.focus = focus;
                self.move_selection(3, view);
            }
            MouseEventKind::ScrollUp => {
                self.focus = focus;
                self.move_selection(-3, view);
            }
            _ => {}
        }
    }
}
fn nonempty(text: &str) -> Option<String> {
    let text = text.trim();
    (!text.is_empty()).then(|| text.into())
}
