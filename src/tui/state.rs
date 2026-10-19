use super::{
    command::{self, Command, CommandSender, Parsed},
    worker::View,
};
use crate::model::{Filter, Station};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{layout::Rect, widgets::ListState};
use std::{
    collections::VecDeque,
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
    pub fn preference(self) -> &'static str {
        match self {
            Self::Stations => "stations",
            Self::Favorites => "favorites",
            Self::History => "history",
            Self::Recordings => "recordings",
        }
    }
    pub fn restore(store: &crate::store::Store, resume: bool) -> anyhow::Result<Self> {
        if !resume {
            return Ok(Self::Stations);
        }
        Ok(match store.meta("pref_source")?.as_deref() {
            Some("favorites") => Self::Favorites,
            Some("history") => Self::History,
            Some("recordings") => Self::Recordings,
            _ => Self::Stations,
        })
    }
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
    pub station_ids: Vec<String>,
    pub recording_ids: Vec<String>,
    history_path: Option<std::path::PathBuf>,
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
    pub fn remember(&mut self) -> anyhow::Result<()> {
        if !self.text.trim().is_empty() && self.history.back() != Some(&self.text) {
            self.history.push_back(self.text.clone());
        }
        while self.history.len() > 100 {
            self.history.pop_front();
        }
        self.save_history()
    }
    pub fn load_history(&mut self, path: std::path::PathBuf) -> anyhow::Result<()> {
        use std::io::Read;
        self.history_path = Some(path.clone());
        match std::fs::File::open(&path) {
            Ok(file) => {
                let mut bytes = Vec::new();
                file.take(512 * 1024).read_to_end(&mut bytes)?;
                let entries: Vec<String> = serde_json::from_slice(&bytes)?;
                self.history = entries
                    .into_iter()
                    .filter(|s| s.len() <= 4096)
                    .rev()
                    .take(100)
                    .collect();
                self.history.make_contiguous().reverse();
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        self.history_path = Some(path);
        Ok(())
    }
    fn save_history(&self) -> anyhow::Result<()> {
        use std::{io::Write, os::unix::fs::OpenOptionsExt};
        let Some(path) = &self.history_path else {
            return Ok(());
        };
        let temp = path.with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
        let result = (|| -> anyhow::Result<()> {
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&temp)?;
            file.write_all(&serde_json::to_vec(&self.history)?)?;
            std::fs::rename(&temp, path)?;
            Ok(())
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(temp);
        }
        result
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
        let choices =
            command::completion_candidates(&self.text, &self.station_ids, &self.recording_ids);
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
    pub mouse_enabled: bool,
    pub clipboard: Option<String>,
    pub filter_input: Editor,
    pub theme: crate::config::Theme,
    pub recording_error_expanded: bool,
    pub confirmation: Option<Command>,
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
    pub facet_index: Option<usize>,
    pub facet_prefix: String,
    backups_request: Option<(String, u64, Instant)>,
    pub help: bool,
    pub help_content: Option<String>,
    pub help_scroll: u16,
    pub details_scroll: u16,
    pub inspect_info: bool,
    pub info_request: u64,
    pub activity_scroll: u16,
    pub rects: [Rect; 5],
    pub filter_summary: Rect,
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
            mouse_enabled: true,
            clipboard: None,
            filter_input: Editor::default(),
            confirmation: None,
            theme: crate::config::Theme::Default,
            recording_error_expanded: false,
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
            facet_index: None,
            facet_prefix: String::new(),
            backups_request: None,
            help: false,
            help_content: None,
            help_scroll: 0,
            details_scroll: 0,
            inspect_info: false,
            info_request: 0,
            activity_scroll: 0,
            rects: [Rect::default(); 5],
            filter_summary: Rect::default(),
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
    pub fn update_size(&mut self, width: u16, height: u16) {
        self.too_small = width < 40 || height < 16;
    }

    pub fn selected<'a>(&self, view: &'a View) -> Option<&'a Station> {
        if self.source == Source::Recordings || self.filter != view.filter {
            None
        } else {
            view.rows.get(self.list.selected().unwrap_or(0))
        }
    }
    pub fn length(&self, view: &View) -> usize {
        if self.filter != view.filter {
            0
        } else if self.source == Source::Recordings {
            view.recordings.len()
        } else {
            view.rows.len()
        }
    }
    pub fn reconcile(&mut self, view: &View) {
        let len = self.length(view);
        if self.source != Source::Recordings && self.filter != view.filter {
            return;
        }
        let revision = if self.source == Source::Recordings {
            view.recordings_revision
        } else {
            view.rows_revision
        };
        if revision != self.rows_revision {
            if let Some(id) = &self.selected_id
                && let Some(i) = if self.source == Source::Recordings {
                    view.recordings.iter().position(|r| &r.id == id)
                } else {
                    view.rows.iter().position(|s| &s.stationuuid == id)
                }
            {
                self.list.select(Some(i));
            }
            self.rows_revision = revision;
        }
        self.list.select(if len == 0 {
            None
        } else {
            Some(self.list.selected().unwrap_or(0).min(len - 1))
        });
        self.selected_id = self.selection_id(view);
    }
    fn selection_id(&self, view: &View) -> Option<String> {
        if self.source == Source::Recordings {
            view.recordings
                .get(self.list.selected().unwrap_or(0))
                .map(|r| r.id.clone())
        } else {
            self.selected(view).map(|s| s.stationuuid.clone())
        }
    }
    pub fn request_selected_backups(&mut self, view: &View, sender: &CommandSender) {
        self.request_selected_backups_at(view, sender, Instant::now());
    }
    pub(super) fn request_selected_backups_at(
        &mut self,
        view: &View,
        sender: &CommandSender,
        now: Instant,
    ) {
        let station = if self.inspect_info && view.inspected_request == self.info_request {
            view.inspected.as_ref()
        } else {
            self.selected(view)
        };
        if let Some(station) = station {
            let request = (station.stationuuid.clone(), view.rows_revision);
            if !view.backups.contains_key(&station.stationuuid)
                && self
                    .backups_request
                    .as_ref()
                    .is_none_or(|(uuid, revision, sent)| {
                        (uuid, *revision) != (&request.0, request.1)
                            || view.backups_errors.contains_key(uuid)
                                && now.saturating_duration_since(*sent) >= Duration::from_secs(2)
                    })
                && self.send(sender, Command::Backups(station.stationuuid.clone()))
            {
                self.backups_request = Some((request.0, request.1, now));
            }
        }
    }
    pub fn facet_candidates<'a>(
        &self,
        view: &'a View,
        selected: usize,
    ) -> Vec<&'a crate::model::Facet> {
        let prefix = self.facet_prefix.trim().to_lowercase();
        view.facets[selected]
            .iter()
            .filter(|facet| facet.value.to_lowercase().starts_with(&prefix))
            .collect()
    }
    fn request_info(&mut self, uuid: String, sender: &CommandSender) {
        self.info_request = self.info_request.wrapping_add(1);
        self.inspect_info = self.send(sender, Command::Info(uuid, self.info_request));
        self.focus = Focus::Details;
        self.details_scroll = 0;
    }
    fn send(&mut self, sender: &CommandSender, command: Command) -> bool {
        match sender.try_send(command) {
            Ok(()) => true,
            Err(error) => {
                self.message = format!("无法提交操作，请稍后重试: {error}");
                false
            }
        }
    }
    pub fn flush_search(&mut self, sender: &CommandSender) {
        if self.send(
            sender,
            if self.source == Source::Recordings {
                Command::Recordings(self.filter.clone())
            } else {
                Command::Search(self.filter.clone())
            },
        ) {
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

    fn clear_filters(&mut self, sender: &CommandSender) {
        self.filter = Filter {
            favorites: self.source == Source::Favorites,
            history: self.source == Source::History,
            recordings: self.source == Source::Recordings,
            limit: self.filter.limit,
            sort: self.filter.sort,
            ..Filter::default()
        };
        self.filter_edit = None;
        self.reset_selection();
        self.flush_search(sender);
        self.message = "已清空筛选".into();
    }
    fn switch_source(
        &mut self,
        source: Source,
        page: Option<crate::cli::Page>,
        sender: &CommandSender,
    ) {
        self.positions[self.source.index()] = self.list.selected().unwrap_or(0);
        self.source = source;
        self.list = ListState::default().with_selected(Some(self.positions[source.index()]));
        self.selected_id = None;
        self.details_scroll = 0;
        self.inspect_info = false;
        self.filter.favorites = source == Source::Favorites;
        self.filter.history = source == Source::History;
        self.filter.recordings = source == Source::Recordings;
        self.filter.offset = page.as_ref().map_or(0, |p| p.offset);
        if let Some(page) = page {
            self.filter.limit = usize::from(page.limit);
        }
        self.flush_search(sender);
    }
    fn focus_direction(&mut self, dx: i32, dy: i32) {
        if self.rects[Focus::List.index()] == self.rects[Focus::Details.index()] && dy == 0 {
            match (self.focus, dx) {
                (Focus::List, 1) => {
                    self.focus = Focus::Details;
                    return;
                }
                (Focus::Details, -1) => {
                    self.focus = Focus::List;
                    return;
                }
                _ => {}
            }
        }
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
                self.selected_id = self.selection_id(view);
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
    fn execute_line(&mut self, view: &View, sender: &CommandSender) {
        match command::parse(
            &self.editor.text,
            self.selected(view),
            view.status.station.as_ref(),
            &self.filter,
        ) {
            Ok(parsed) => {
                self.mode = Mode::Normal;
                self.message.clear();
                if let Err(error) = self.editor.remember() {
                    self.message = format!("历史保存失败: {error}");
                }
                // A command supersedes any pending incremental search.
                if self.search_due.is_some()
                    && !matches!(
                        &parsed,
                        Parsed::Search(_) | Parsed::Source(..) | Parsed::Quit
                    )
                {
                    self.flush_search(sender);
                }
                match parsed {
                    Parsed::Mouse(enabled) => {
                        self.mouse_enabled = enabled;
                        self.message = format!("mouse = {enabled}");
                    }
                    Parsed::HelpText(text) => {
                        self.help = true;
                        self.help_content = Some(text);
                        self.help_scroll = 0;
                    }
                    Parsed::Quit => self.quit = true,
                    Parsed::Help => {
                        self.help_content = None;
                        self.help = true;
                        self.help_scroll = 0;
                    }
                    Parsed::Focus(focus) => self.focus = focus,
                    Parsed::Source(source, page) => {
                        self.switch_source(source, Some(page), sender);
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
                        self.flush_search(sender);
                    }
                    Parsed::Worker(mut command) => {
                        if matches!(
                            command,
                            Command::Station(crate::cli::StationAction::Remove { .. })
                                | Command::DeleteRecording { .. }
                        ) {
                            self.confirmation = Some(command);
                            self.message =
                                "确认删除？录音默认先删文件再删索引；--index-only 保留文件。y 确认 · n/Esc 取消".into();
                            return;
                        }
                        if let Command::Info(uuid, _) = command {
                            self.request_info(uuid, sender);
                            return;
                        }
                        if matches!(command, Command::Random(_))
                            && self.source == Source::Recordings
                        {
                            if let Command::Random(filter) = &mut command {
                                filter.recordings = false;
                            }
                            self.switch_source(Source::Stations, None, sender);
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
    pub fn key(&mut self, key: KeyEvent, view: &View, sender: &CommandSender) {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c')
            || self.too_small && key.code == KeyCode::Char('q')
        {
            self.quit = true;
            return;
        }
        if let Some(command) = self.confirmation.take() {
            match key.code {
                KeyCode::Char('y') => {
                    if self.send(sender, command) {
                        self.message = "删除命令已提交".into();
                    }
                }
                KeyCode::Char('n') | KeyCode::Esc => self.message.clear(),
                _ => self.confirmation = Some(command),
            }
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
            if self.mode == Mode::Command && matches!(key.code, KeyCode::Tab | KeyCode::BackTab) {
                self.editor.station_ids = self
                    .selected(view)
                    .into_iter()
                    .chain(view.status.station.as_ref())
                    .map(|s| s.stationuuid.clone())
                    .collect();
                self.editor.recording_ids = view
                    .recordings
                    .iter()
                    .take(20)
                    .map(|r| r.id.clone())
                    .collect();
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
                    self.help_scroll = self.help_scroll.saturating_add(1).min(4096)
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    self.help_scroll = self.help_scroll.saturating_sub(1)
                }
                KeyCode::PageDown => {
                    self.help_scroll = self.help_scroll.saturating_add(10).min(4096)
                }
                KeyCode::PageUp => self.help_scroll = self.help_scroll.saturating_sub(10),
                _ => {}
            }
            return;
        }
        if self.filter_edit.is_some()
            && key.modifiers.contains(KeyModifiers::CONTROL)
            && key.code == KeyCode::Char('u')
        {
            self.clear_filters(sender);
            return;
        }
        if let Some((selected, values)) = &mut self.filter_edit {
            if self.filter_input.text != values[*selected] {
                self.filter_input.set(&values[*selected]);
            }
            match key.code {
                KeyCode::Down | KeyCode::Up if *selected < 3 => {
                    let prefix = self.facet_prefix.trim().to_lowercase();
                    let candidates = view.facets[*selected]
                        .iter()
                        .filter(|facet| facet.value.to_lowercase().starts_with(&prefix))
                        .collect::<Vec<_>>();
                    if !candidates.is_empty() {
                        let index = match self.facet_index {
                            Some(index) if key.code == KeyCode::Down => {
                                (index + 1) % candidates.len()
                            }
                            Some(index) => (index + candidates.len() - 1) % candidates.len(),
                            None if key.code == KeyCode::Down => 0,
                            None => candidates.len() - 1,
                        };
                        self.facet_index = Some(index);
                        values[*selected] = candidates[index].value.clone();
                        self.filter_input.set(&values[*selected]);
                    }
                }
                KeyCode::Esc => self.filter_edit = None,
                KeyCode::Tab => {
                    *selected = (*selected + 1) % 4;
                    self.facet_index = None;
                    self.filter_input.set(&values[*selected]);
                    self.facet_prefix = values[*selected].clone();
                }
                KeyCode::BackTab => {
                    *selected = (*selected + 3) % 4;
                    self.facet_index = None;
                    self.filter_input.set(&values[*selected]);
                    self.facet_prefix = values[*selected].clone();
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
                        self.flush_search(sender);
                    }
                    Err(_) => self.message = "最低码率必须为非负整数".into(),
                },
                _ => {
                    self.filter_input.key(key, false);
                    values[*selected] = self.filter_input.text.clone();
                    self.facet_prefix = self.filter_input.text.clone();
                    self.facet_index = None;
                }
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
                let mut errors = vec![];
                if let crate::engine::PlaybackState::Failed { reason } = &view.status.playback_state
                {
                    errors.push(format!("播放失败\n{reason}"));
                }
                if let crate::engine::RecordingState::Failed { reason } =
                    &view.status.recording_state
                {
                    errors.push(format!("录音失败\n{reason}"));
                }
                self.help_content = (!errors.is_empty())
                    .then(|| format!("{}\n\nEsc / ? 关闭；j/k 滚动。", errors.join("\n\n")));
                self.help = true;
                self.help_scroll = 0;
            }
            KeyCode::Char('q') => self.quit = true,
            KeyCode::Tab => self.focus = Focus::ALL[(self.focus.index() + 1) % 5],
            KeyCode::BackTab => self.focus = Focus::ALL[(self.focus.index() + 4) % 5],
            KeyCode::Char(c @ '1'..='5') => self.focus = Focus::ALL[c as usize - '1' as usize],
            KeyCode::Char('[') => {
                self.switch_source(Source::ALL[(self.source.index() + 3) % 4], None, sender)
            }
            KeyCode::Char(']') => {
                self.switch_source(Source::ALL[(self.source.index() + 1) % 4], None, sender)
            }
            KeyCode::Up | KeyCode::Char('k') if self.focus == Focus::Sources => {
                self.switch_source(Source::ALL[(self.source.index() + 3) % 4], None, sender)
            }
            KeyCode::Down | KeyCode::Char('j') if self.focus == Focus::Sources => {
                self.switch_source(Source::ALL[(self.source.index() + 1) % 4], None, sender)
            }
            KeyCode::Down | KeyCode::Char('j') => self.move_selection(1, view),
            KeyCode::Up | KeyCode::Char('k') => self.move_selection(-1, view),
            KeyCode::Char('h') | KeyCode::Left => self.focus_direction(-1, 0),
            KeyCode::Char('l') | KeyCode::Right => self.focus_direction(1, 0),
            KeyCode::Home => self.move_selection(-10000, view),
            KeyCode::Char('g') if gg => self.move_selection(-10000, view),
            KeyCode::Char('g') => self.pending_g = true,
            KeyCode::End | KeyCode::Char('G') => self.move_selection(10000, view),
            KeyCode::PageDown | KeyCode::PageUp if self.focus == Focus::List => {
                if self.filter != view.filter {
                    return;
                }
                let total = if self.source == Source::Recordings {
                    view.recordings_total
                } else {
                    view.total
                };
                let next = self.filter.offset.saturating_add(self.filter.limit);
                if key.code == KeyCode::PageDown && next < total {
                    self.filter.offset = next;
                } else if key.code == KeyCode::PageUp {
                    self.filter.offset = self.filter.offset.saturating_sub(self.filter.limit);
                } else {
                    return;
                }
                self.reset_selection();
                self.flush_search(sender);
            }
            KeyCode::PageDown => self.move_selection(10, view),
            KeyCode::PageUp => self.move_selection(-10, view),
            KeyCode::Enter if self.focus == Focus::Sources => self.focus = Focus::List,
            KeyCode::Enter if self.focus == Focus::Player => {
                if let Some(station) = &view.status.station {
                    self.request_info(station.stationuuid.clone(), sender);
                }
            }
            KeyCode::Enter if self.focus == Focus::List => {
                if self.source == Source::Recordings && self.filter == view.filter {
                    if let Some(r) = view.recordings.get(self.list.selected().unwrap_or(0)) {
                        self.send(sender, Command::PlayRecording(r.id.clone()));
                    }
                } else if let Some(s) = self.selected(view) {
                    self.send(sender, Command::Play(s.stationuuid.clone()));
                }
            }
            KeyCode::Char('D')
                if self.source == Source::Recordings && self.filter == view.filter =>
            {
                if let Some(r) = view.recordings.get(self.list.selected().unwrap_or(0)) {
                    self.confirmation = Some(Command::DeleteRecording {
                        id: r.id.clone(),
                        index_only: false,
                    });
                    self.message = "删除录音文件、sidecar 和索引？y 确认 · n/Esc 取消；仅删索引用 :recordings remove ID --index-only".into();
                }
            }
            KeyCode::Char('o') if self.source == Source::Recordings => {
                self.send(sender, Command::OpenRecordings);
            }
            KeyCode::Char('e') if self.source == Source::Recordings => {
                self.recording_error_expanded = !self.recording_error_expanded;
                self.focus = Focus::Details;
                self.details_scroll = 0;
            }
            KeyCode::Char('f') if self.source != Source::Recordings => {
                if let Some(s) = self.selected(view) {
                    self.send(sender, Command::ToggleFavorite(s.stationuuid.clone()));
                }
            }
            KeyCode::Char('F') if self.source != Source::Recordings => {
                self.filter_input
                    .set(self.filter.country.as_deref().unwrap_or(""));
                self.facet_index = None;
                self.facet_prefix = self.filter_input.text.clone();
                self.send(sender, Command::Facets);
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
            KeyCode::F(5) => {
                self.flush_search(sender);
            }
            KeyCode::Char('n') => {
                if self.source == Source::Recordings {
                    self.switch_source(Source::Stations, None, sender);
                }
                self.send(sender, Command::Random(self.filter.clone()));
            }
            KeyCode::Char('r') => {
                self.send(sender, Command::RecordToggle);
            }
            KeyCode::Char('m') => {
                self.send(sender, Command::Mute);
            }
            KeyCode::Char('y') | KeyCode::Char('Y') => {
                self.clipboard = if self.source == Source::Recordings && self.filter == view.filter
                {
                    view.recordings
                        .get(self.list.selected().unwrap_or(0))
                        .map(|r| {
                            if key.code == KeyCode::Char('Y') {
                                r.path.clone()
                            } else {
                                r.id.clone()
                            }
                        })
                } else {
                    self.selected(view).map(|s| {
                        if key.code == KeyCode::Char('Y') {
                            s.primary_url().into()
                        } else {
                            s.stationuuid.clone()
                        }
                    })
                };
                if self.clipboard.is_some() {
                    self.message = "已发送到终端剪贴板".into();
                }
            }
            KeyCode::Char(' ') => {
                self.send(sender, Command::Pause);
            }
            KeyCode::Char('+') | KeyCode::Char('=') => {
                self.send(sender, Command::AdjustVolume(5));
            }
            KeyCode::Char('-') => {
                self.send(sender, Command::AdjustVolume(-5));
            }
            _ => {}
        }
    }
    pub fn mouse(&mut self, event: MouseEvent, view: &View, sender: &CommandSender) {
        if !self.mouse_enabled
            || self.mode != Mode::Normal
            || self.help
            || self.filter_edit.is_some()
            || self.confirmation.is_some()
        {
            return;
        }
        let point = ratatui::layout::Position::new(event.column, event.row);
        if self.filter_summary.contains(point)
            && event.kind == MouseEventKind::Down(MouseButton::Left)
        {
            self.clear_filters(sender);
            return;
        }
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
                        self.switch_source(source, None, sender);
                    }
                } else if focus == Focus::List {
                    let rect = self.rects[focus.index()];
                    if event.row > rect.y && event.row < rect.bottom().saturating_sub(1) {
                        let index = self.list.offset() + usize::from(event.row - rect.y - 1);
                        if index < self.length(view) {
                            self.list.select(Some(index));
                            self.selected_id = self.selection_id(view);
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
