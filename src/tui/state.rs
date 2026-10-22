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
            Self::Sources => crate::t!("focus_sources"),
            Self::List => crate::t!("focus_list"),
            Self::Details => crate::t!("focus_details"),
            Self::Player => crate::t!("focus_player"),
            Self::Activity => crate::t!("focus_activity"),
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
    pub fn restore(
        store: &crate::store::Store,
        resume: crate::config::ResumeMode,
    ) -> anyhow::Result<Self> {
        if resume == crate::config::ResumeMode::Disabled {
            return Ok(Self::Stations);
        }
        Ok(match store.meta("pref_source")?.as_deref() {
            Some("favorites") => Self::Favorites,
            Some("history") => Self::History,
            Some("recordings") => Self::Recordings,
            _ => Self::Stations,
        })
    }
    pub fn initial_filter(
        self,
        store: &crate::store::Store,
        resume: crate::config::ResumeMode,
    ) -> anyhow::Result<(Filter, Option<String>)> {
        let mut filter = Filter {
            favorites: self == Self::Favorites,
            history: self == Self::History,
            recordings: self == Self::Recordings,
            limit: 100,
            ..Filter::default()
        };
        let mut selected = None;
        if resume == crate::config::ResumeMode::Selection
            && self != Self::Recordings
            && let Some(uuid) = store.meta("pref_last_station")?
            && let Some(position) = store.station_position(&uuid, &filter)?
        {
            filter.offset = position / filter.limit * filter.limit;
            selected = Some(uuid);
        }
        Ok((filter, selected))
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
    pub fn short_label(self) -> &'static str {
        match self {
            Self::Stations => crate::t!("source_stations_short"),
            Self::Favorites => crate::t!("source_favorites_short"),
            Self::History => crate::t!("source_history_short"),
            Self::Recordings => crate::t!("source_recordings_short"),
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Stations => crate::t!("source_stations"),
            Self::Favorites => crate::t!("source_favorites"),
            Self::History => crate::t!("source_history"),
            Self::Recordings => crate::t!("source_recordings"),
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Mode {
    Normal,
    Search,
    Command,
}

/// Largest command, search or filter line the editor accepts, in bytes.
const EDITOR_LIMIT: usize = 2048;
/// How long a plain notice stays in place of the context hint.
const MESSAGE_TTL: Duration = Duration::from_secs(4);

/// Wrapped length and visible height of a scrolling text area, as measured by
/// the most recent frame; the scroll offset may never pass `max`.
#[derive(Clone, Copy, Default)]
pub(super) struct ScrollExtent {
    pub total: usize,
    pub visible: usize,
}
impl ScrollExtent {
    pub fn max(self) -> usize {
        self.total.saturating_sub(self.visible)
    }
    /// The 1-based first and last visible line, or `None` when everything fits.
    pub fn span(self, scroll: u16) -> Option<(usize, usize)> {
        (self.max() > 0).then(|| {
            let first = usize::from(scroll).min(self.max());
            (first + 1, (first + self.visible).min(self.total))
        })
    }
}

#[derive(Clone, Copy)]
enum PopupMove {
    Lines(isize),
    Pages(isize),
    Top,
    Bottom,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum Remembered {
    Kept,
    WithheldCredentials,
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
    /// Records a submitted line for Up/Down recall and the history file. A line
    /// holding URL credentials is withheld from both, never rewritten.
    pub fn remember(&mut self) -> anyhow::Result<Remembered> {
        if crate::redact::has_url_credentials(&self.text) {
            return Ok(Remembered::WithheldCredentials);
        }
        if !self.text.trim().is_empty() && self.history.back() != Some(&self.text) {
            self.history.push_back(self.text.clone());
        }
        while self.history.len() > 100 {
            self.history.pop_front();
        }
        self.save_history()?;
        Ok(Remembered::Kept)
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
        if command && matches!(key.code, KeyCode::Tab | KeyCode::BackTab) {
            self.complete(key.code == KeyCode::BackTab);
            return;
        }
        self.completion = None;
        match key.code {
            KeyCode::Left => self.cursor = super::text::prev_boundary(&self.text, self.cursor),
            KeyCode::Right => self.cursor = super::text::next_boundary(&self.text, self.cursor),
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
                let start = super::text::prev_boundary(&self.text, self.cursor);
                self.text.drain(start..self.cursor);
                self.cursor = start;
            }
            KeyCode::Delete => {
                let end = super::text::next_boundary(&self.text, self.cursor);
                self.text.drain(self.cursor..end);
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
                    && self.text.len() < EDITOR_LIMIT =>
            {
                self.text.insert(self.cursor, c);
                self.cursor += c.len_utf8();
            }
            _ => {}
        }
    }
    /// Insert pasted text at the cursor. Line breaks and tabs become spaces so a
    /// paste can never submit the line, other control characters are dropped,
    /// and the result honors the same size limit as typing.
    pub fn paste(&mut self, text: &str) {
        let mut clean = String::new();
        let mut chars = text.chars().peekable();
        while let Some(c) = chars.next() {
            match c {
                '\r' => {
                    chars.next_if_eq(&'\n');
                    clean.push(' ');
                }
                '\n' | '\t' => clean.push(' '),
                c if c.is_control() => {}
                c => clean.push(c),
            }
            if clean.len() >= EDITOR_LIMIT {
                break;
            }
        }
        let room = EDITOR_LIMIT.saturating_sub(self.text.len());
        let clean = super::text::prefix_bytes(&clean, room);
        if clean.is_empty() {
            return;
        }
        self.completion = None;
        self.text.insert_str(self.cursor, clean);
        self.cursor += clean.len();
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

pub(super) struct Confirmation {
    command: Command,
    pub id: String,
    pub summary: String,
    pub scope: &'static str,
    pub error: String,
}
impl Confirmation {
    fn new(command: Command, view: &View) -> Option<Self> {
        let (id, summary, scope) = match &command {
            Command::DeleteRecording { id, index_only } => (
                id.clone(),
                view.recordings
                    .iter()
                    .find(|r| &r.id == id)
                    .map(|r| r.station_name.as_str()),
                if *index_only {
                    crate::t!("confirm_index_scope")
                } else {
                    crate::t!("confirm_recording_scope")
                },
            ),
            Command::Station(crate::cli::StationAction::Remove {
                uuid,
                keep_references,
            }) => (
                uuid.clone(),
                view.rows
                    .iter()
                    .chain(view.inspected.iter())
                    .chain(view.status.station.iter())
                    .find(|s| &s.stationuuid == uuid)
                    .map(|s| s.name.as_str()),
                if *keep_references {
                    crate::t!("confirm_station_keep_scope")
                } else {
                    crate::t!("confirm_station_scope")
                },
            ),
            _ => return None,
        };
        Some(Self {
            command,
            id,
            summary: crate::clean_text(summary.unwrap_or_default()),
            scope,
            error: String::new(),
        })
    }
    fn command(&self) -> Command {
        self.command.clone()
    }
}

/// Where the list was when `/` opened the search box, so Esc can put it back.
struct SearchOrigin {
    filter: Filter,
    selected_id: Option<String>,
    index: usize,
    edited: bool,
}

/// Where a source's list was when the user switched away from it; kept for the
/// session only and unrelated to the `resume` setting.
struct SourcePosition {
    filter: Filter,
    selected_id: Option<String>,
}

pub(super) struct App {
    last_click: Option<(Instant, u16, u16, String, u64)>,
    pub jump_binding: String,
    jump_progress: usize,
    jump_last: Option<Instant>,
    jump_request: u64,
    pending_jump: Option<(u64, String)>,
    jump_selection: Option<(String, u64)>,
    pub mouse_enabled: bool,
    pub clipboard: Option<String>,
    pub filter_input: Editor,
    pub theme: crate::config::Theme,
    pub artwork: super::artwork::ImageState,
    pub recording_error_expanded: bool,
    pub confirmation: Option<Confirmation>,
    pub focus: Focus,
    pub source: Source,
    pub mode: Mode,
    pub filter: Filter,
    pub list: ListState,
    pub editor: Editor,
    pub message: String,
    message_clock: Option<(String, Instant)>,
    pub search_due: Option<Instant>,
    search_origin: Option<SearchOrigin>,
    pub filter_edit: Option<(usize, [String; 4])>,
    pub facet_index: Option<usize>,
    pub facet_prefix: String,
    backups_request: Option<(String, u64, Instant, u32)>,
    facets_request: Option<(usize, String)>,
    facet_due: Option<Instant>,
    pub help: bool,
    pub help_content: Option<String>,
    pub help_title: Option<&'static str>,
    pub help_scroll: u16,
    pub popup: ScrollExtent,
    pub details_scroll: u16,
    pub inspect_info: bool,
    pub info_request: u64,
    pub activity_scroll: u16,
    pub rects: [Rect; 5],
    pub filter_summary: Rect,
    pub source_rows: [Rect; 4],
    positions: [Option<SourcePosition>; 4],
    pub quit: bool,
    pub too_small: bool,
    size: (u16, u16),
    pub dismissed_error: u64,
    pending_g: bool,
    rows_revision: u64,
    selected_id: Option<String>,
    restore_selection: Option<String>,
}
impl Default for App {
    fn default() -> Self {
        let mut list = ListState::default();
        list.select(Some(0));
        Self {
            last_click: None,
            jump_binding: "''".into(),
            jump_progress: 0,
            jump_last: None,
            jump_request: 0,
            pending_jump: None,
            jump_selection: None,
            mouse_enabled: true,
            clipboard: None,
            filter_input: Editor::default(),
            confirmation: None,
            theme: crate::config::Theme::Default,
            artwork: Default::default(),
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
            message_clock: None,
            search_due: None,
            search_origin: None,
            filter_edit: None,
            facet_index: None,
            facet_prefix: String::new(),
            backups_request: None,
            facets_request: None,
            facet_due: None,
            help: false,
            help_content: None,
            help_title: None,
            help_scroll: 0,
            popup: ScrollExtent::default(),
            details_scroll: 0,
            inspect_info: false,
            info_request: 0,
            activity_scroll: 0,
            rects: [Rect::default(); 5],
            filter_summary: Rect::default(),
            source_rows: [Rect::default(); 4],
            positions: Default::default(),
            quit: false,
            too_small: false,
            size: (0, 0),
            dismissed_error: 0,
            pending_g: false,
            rows_revision: 0,
            selected_id: None,
            restore_selection: None,
        }
    }
}
impl App {
    pub fn restore(
        store: &crate::store::Store,
        resume: crate::config::ResumeMode,
    ) -> anyhow::Result<Self> {
        let source = Source::restore(store, resume)?;
        let (filter, restore_selection) = source.initial_filter(store, resume)?;
        Ok(Self {
            source,
            filter,
            restore_selection,
            ..Self::default()
        })
    }
    pub fn set_rects(&mut self, rects: [Rect; 5]) {
        if self.rects != rects {
            self.last_click = None;
        }
        self.rects = rects;
    }
    /// Moves the text popup within the bounds measured by the last frame. Until a
    /// frame has measured it the bound is open and the renderer clamps afterwards.
    fn move_popup(&mut self, movement: PopupMove) {
        let limit = usize::from(u16::MAX);
        let max = if self.popup.visible == 0 {
            limit
        } else {
            self.popup.max().min(limit)
        };
        let page = isize::try_from(self.popup.visible.max(1)).unwrap_or(isize::MAX);
        let current = usize::from(self.help_scroll);
        let next = match movement {
            PopupMove::Lines(count) => current.saturating_add_signed(count),
            PopupMove::Pages(count) => current.saturating_add_signed(count.saturating_mul(page)),
            PopupMove::Top => 0,
            PopupMove::Bottom => max,
        };
        self.help_scroll = next.min(max) as u16;
    }
    /// Clears an ordinary notice after `MESSAGE_TTL` of being on screen and
    /// reports whether it did. Notices that belong to an open dialog or an
    /// unfinished locate stay until their owner clears them, and the clock only
    /// starts once the notice is actually visible.
    pub fn expire_message(&mut self, now: Instant) -> bool {
        let visible = self.mode == Mode::Normal
            && !self.help
            && !self.too_small
            && self.confirmation.is_none()
            && self.filter_edit.is_none()
            && self.pending_jump.is_none()
            && self.jump_selection.is_none();
        if self.message.is_empty() || !visible {
            self.message_clock = None;
            return false;
        }
        match &self.message_clock {
            Some((text, since)) if *text == self.message => {
                if now.saturating_duration_since(*since) < MESSAGE_TTL {
                    return false;
                }
                self.message.clear();
                self.message_clock = None;
                true
            }
            _ => {
                self.message_clock = Some((self.message.clone(), now));
                false
            }
        }
    }
    pub fn update_size(&mut self, width: u16, height: u16) {
        if self.size != (width, height) {
            self.last_click = None;
            self.rects = [Rect::default(); 5];
            self.source_rows = [Rect::default(); 4];
            self.filter_summary = Rect::default();
            self.size = (width, height);
        }
        self.too_small = width < 40 || height < 16;
        if self.too_small {
            self.confirmation = None;
            self.message.clear();
            // Preserve editor/filter drafts and deferred searches until visible.
            self.pending_g = false;
            self.jump_progress = 0;
            self.jump_last = None;
            self.last_click = None;
        }
    }

    pub fn selected<'a>(&self, view: &'a View) -> Option<&'a Station> {
        if self.source == Source::Recordings {
            None
        } else {
            view.stations_for(&self.filter)
                .get(self.list.selected().unwrap_or(0))
        }
    }
    pub fn length(&self, view: &View) -> usize {
        if self.source == Source::Recordings {
            view.recordings_for(&self.filter).len()
        } else {
            view.stations_for(&self.filter).len()
        }
    }
    pub fn reconcile(&mut self, view: &View) {
        if view.page.as_ref().is_some_and(|p| {
            p.requested == self.filter && p.phase == super::worker::PagePhase::Ready
        }) && self.filter != view.filter
        {
            self.filter = view.filter.clone();
            self.reset_selection();
        }
        if let Some((selected, values)) = &self.filter_edit
            && *selected < 3
            && self.facet_index.is_some()
        {
            self.facet_index = matching_facets(&view.facets[*selected], &self.facet_prefix)
                .iter()
                .position(|facet| facet.value == values[*selected]);
        }

        if let Some((request, _)) = &self.pending_jump
            && let Some(located) = &view.located
            && *request == located.request
        {
            let (_, uuid) = self.pending_jump.take().unwrap();
            match &located.filter {
                Ok(filter) => {
                    self.source = Source::Stations;
                    self.filter = filter.clone();
                    self.jump_selection = Some((uuid.clone(), view.rows_revision));
                    self.selected_id = Some(uuid);
                    self.list = ListState::default().with_selected(Some(0));
                    self.rows_revision = u64::MAX;
                    self.inspect_info = false;
                    self.details_scroll = 0;
                    self.search_due = Some(Instant::now());
                    self.message = crate::t!("jump_loading").into();
                }
                Err(error) => self.message = crate::clean_text(error),
            }
        }

        if let Some((_, revision)) = &self.jump_selection {
            if !view.page_ready(&self.filter) || *revision == view.rows_revision {
                return;
            }
            let (uuid, _) = self.jump_selection.take().unwrap();
            if let Some(index) = view.rows.iter().position(|row| row.stationuuid == uuid) {
                self.list.select(Some(index));
                if self.message == crate::t!("jump_loading") {
                    self.message = located_notice(view);
                }
            } else {
                self.message = crate::t!("jump_unavailable").into();
            }
        }
        let len = self.length(view);
        if !view.page_ready(&self.filter) {
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
        if view.rows_revision > 0
            && let Some(uuid) = self.restore_selection.take()
            && let Some(index) = view.rows.iter().position(|row| row.stationuuid == uuid)
        {
            self.list.select(Some(index));
        }
        self.selected_id = self.selection_id(view);
    }
    fn selection_id(&self, view: &View) -> Option<String> {
        if self.source == Source::Recordings {
            view.recordings_for(&self.filter)
                .get(self.list.selected().unwrap_or(0))
                .map(|r| r.id.clone())
        } else {
            self.selected(view).map(|s| s.stationuuid.clone())
        }
    }
    pub fn request_selected_backups(&mut self, view: &View, sender: &CommandSender) {
        if !self.too_small {
            self.request_selected_backups_at(view, sender, Instant::now());
        }
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
            let previous = self
                .backups_request
                .as_ref()
                .filter(|(uuid, revision, _, _)| (uuid, *revision) == (&request.0, request.1));
            let attempt = previous.map_or(0, |(_, _, _, attempt)| attempt.saturating_add(1));
            if !view.backups.contains_key(&station.stationuuid)
                && previous.is_none_or(|(uuid, _, sent, previous_attempt)| {
                    // Back off persistent storage failures, up to one query per 30s.
                    let delay = Duration::from_secs((2_u64 << (*previous_attempt).min(4)).min(30));
                    view.backups_errors.contains_key(uuid)
                        && now.saturating_duration_since(*sent) >= delay
                })
                && self.send(sender, Command::Backups(station.stationuuid.clone()))
            {
                self.backups_request = Some((request.0, request.1, now, attempt));
            }
        }
    }
    pub fn facet_candidates<'a>(
        &self,
        view: &'a View,
        selected: usize,
    ) -> Vec<&'a crate::model::Facet> {
        matching_facets(&view.facets[selected], &self.facet_prefix)
    }
    pub fn request_facets(&mut self, sender: &CommandSender) {
        if self.too_small {
            return;
        }
        let Some((selected, _)) = &self.filter_edit else {
            return;
        };
        if *selected >= 3 || self.facet_due.is_some_and(|due| Instant::now() < due) {
            return;
        }
        let request = (*selected, self.facet_prefix.trim().to_lowercase());
        if self.facets_request.as_ref() != Some(&request)
            && self.send(
                sender,
                Command::Facets {
                    selected: request.0,
                    prefix: request.1.clone(),
                },
            )
        {
            self.facets_request = Some(request);
            self.facet_due = None;
        }
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
                self.message = crate::t!("command_submit_failed", "detail" => error);
                false
            }
        }
    }
    pub fn flush_search(&mut self, sender: &CommandSender) {
        if self.too_small {
            return;
        }
        self.pending_jump = None;
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
        self.restore_selection = None;
        self.jump_selection = None;
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
        self.message = crate::t!("filters_cleared").into();
    }
    fn switch_source(
        &mut self,
        source: Source,
        page: Option<crate::cli::Page>,
        sender: &CommandSender,
    ) {
        self.restore_selection = None;
        self.jump_selection = None;
        self.positions[self.source.index()] = Some(SourcePosition {
            filter: self.filter.clone(),
            selected_id: self.selected_id.clone(),
        });
        self.source = source;
        self.filter.favorites = source == Source::Favorites;
        self.filter.history = source == Source::History;
        self.filter.recordings = source == Source::Recordings;
        // An explicit page starts at its first row; otherwise the source comes
        // back where it was, as long as it is still the same query.
        let mut remembered = None;
        match page {
            Some(page) => {
                self.filter.offset = page.offset;
                self.filter.limit = usize::from(page.limit);
            }
            None => {
                remembered = self.positions[source.index()].take().filter(|saved| {
                    let same = |filter: &Filter| Filter {
                        offset: 0,
                        ..filter.clone()
                    };
                    same(&saved.filter) == same(&self.filter)
                });
                self.filter.offset = remembered.as_ref().map_or(0, |saved| saved.filter.offset);
            }
        }
        self.selected_id = remembered.and_then(|saved| saved.selected_id);
        self.list = ListState::default().with_selected(Some(0));
        // The first page to arrive looks the row up by its ID, even if the
        // revision counter happens to match the previous source's.
        self.rows_revision = u64::MAX;
        self.details_scroll = 0;
        self.inspect_info = false;
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
    /// PgUp/PgDn: turn the catalog page in the list, scroll ten lines elsewhere.
    fn page(&mut self, forward: bool, view: &View, sender: &CommandSender) {
        if self.focus != Focus::List {
            self.move_selection(if forward { 10 } else { -10 }, view);
            return;
        }
        if !view.page_ready(&self.filter) {
            return;
        }
        let total = if self.source == Source::Recordings {
            view.recordings_total
        } else {
            view.total
        };
        let next = self.filter.offset.saturating_add(self.filter.limit);
        if forward && next < total {
            self.filter.offset = next;
        } else if !forward {
            self.filter.offset = self.filter.offset.saturating_sub(self.filter.limit);
        } else {
            return;
        }
        self.reset_selection();
        self.flush_search(sender);
    }
    fn execute_line(&mut self, view: &View, sender: &CommandSender) {
        if self.run_line(view, sender) {
            let notice = crate::t!("history_withheld");
            self.message =
                if self.message.is_empty() || self.message == crate::t!("command_submitted") {
                    notice.into()
                } else {
                    format!("{} · {notice}", self.message)
                };
        }
    }
    /// Runs the command line; returns whether it was kept out of the history
    /// because it contains credentials, so the user can be told why.
    fn run_line(&mut self, view: &View, sender: &CommandSender) -> bool {
        match command::parse(
            &self.editor.text,
            self.selected(view),
            view.status.station.as_ref(),
            &self.filter,
        ) {
            Ok(parsed) => {
                self.mode = Mode::Normal;
                self.message.clear();
                let withheld = match self.editor.remember() {
                    Ok(Remembered::Kept) => false,
                    Ok(Remembered::WithheldCredentials) => true,
                    Err(error) => {
                        self.message = crate::t!("history_save_failed", "detail" => error);
                        false
                    }
                };
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
                        self.message = if enabled {
                            crate::t!("mouse_capture_on")
                        } else {
                            crate::t!("mouse_capture_off")
                        }
                        .into();
                    }
                    Parsed::HelpText(text) => {
                        self.help = true;
                        self.help_content = Some(text);
                        self.help_title = Some(crate::t!("command_help_title"));
                        self.help_scroll = 0;
                    }
                    Parsed::Quit => self.quit = true,
                    Parsed::Help => {
                        self.help_content = None;
                        self.help_title = None;
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
                            self.confirmation = Confirmation::new(command, view);
                            return withheld;
                        }
                        if let Command::Info(uuid, _) = command {
                            self.request_info(uuid, sender);
                            return withheld;
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
                            self.message = crate::t!("command_submitted").into();
                        }
                    }
                }
                withheld
            }
            Err(error) => {
                self.message = crate::clean_text(&format!("{error}"));
                false
            }
        }
    }
    fn jump_to_playing(&mut self, view: &View, sender: &CommandSender) {
        // A failed station is still the one the player header names, so it can
        // be located (never replayed); only an idle player has nothing to find.
        let Some(station) = view
            .status
            .station
            .as_ref()
            .filter(|_| view.status.playback_state != crate::engine::PlaybackState::Idle)
        else {
            self.message = crate::t!("jump_not_playing").into();
            return;
        };
        self.pending_jump = None;
        self.jump_selection = None;
        self.focus = Focus::List;
        self.inspect_info = false;
        self.details_scroll = 0;
        if self.source != Source::Recordings
            && let Some(index) = view
                .stations_for(&self.filter)
                .iter()
                .position(|row| row.stationuuid == station.stationuuid)
        {
            self.list.select(Some(index));
            self.selected_id = Some(station.stationuuid.clone());
            self.message = located_notice(view);
            return;
        }
        let request = self.jump_request.wrapping_add(1);
        if self.send(
            sender,
            Command::LocatePlaying {
                uuid: station.stationuuid.clone(),
                request,
                limit: self.filter.limit,
            },
        ) {
            self.jump_request = request;
            self.pending_jump = Some((request, station.stationuuid.clone()));
            self.search_due = None;
            self.message = crate::t!("jump_loading").into();
        }
    }
    fn jump_key(&mut self, key: KeyEvent, view: &View, sender: &CommandSender) -> bool {
        let Some(sequence) = crate::config::parse_keybinding(&self.jump_binding) else {
            return false;
        };
        if self
            .jump_last
            .is_some_and(|time| time.elapsed() > Duration::from_secs(1))
        {
            self.jump_progress = 0;
        }
        let mut modifiers = key.modifiers;
        let code = if modifiers.contains(KeyModifiers::CONTROL)
            && let KeyCode::Char(c) = key.code
        {
            KeyCode::Char(c.to_ascii_lowercase())
        } else {
            key.code
        };
        if matches!(key.code, KeyCode::Char(_) | KeyCode::BackTab) {
            modifiers.remove(KeyModifiers::SHIFT);
        }
        let stroke = (code, modifiers);
        if sequence.get(self.jump_progress) != Some(&stroke) {
            self.jump_progress = 0;
        }
        if sequence.get(self.jump_progress) == Some(&stroke) {
            self.pending_g = false;
            self.jump_progress += 1;
            self.jump_last = Some(Instant::now());
            if self.jump_progress == sequence.len() {
                self.jump_progress = 0;
                self.jump_to_playing(view, sender);
            }
            return true;
        }
        false
    }
    fn search_edited(&mut self) {
        // Cursor-only keys leave the query alone and must not re-query or
        // reset the selection and page.
        if self.filter.query == self.editor.text {
            return;
        }
        self.filter.query = self.editor.text.clone();
        self.filter.offset = 0;
        self.reset_selection();
        if let Some(origin) = &mut self.search_origin {
            origin.edited = true;
        }
        self.search_due = Some(Instant::now() + Duration::from_millis(200));
    }
    /// Esc in the search box. Nothing changes until the text was edited; after
    /// that the query, page and selected row from before the search return.
    fn cancel_search(&mut self) {
        let Some(origin) = self.search_origin.take() else {
            return;
        };
        if !origin.edited {
            return;
        }
        self.filter = origin.filter;
        self.list = ListState::default().with_selected(Some(origin.index));
        self.selected_id = origin.selected_id;
        self.details_scroll = 0;
        self.inspect_info = false;
        self.search_due = Some(Instant::now());
    }
    /// Bracketed paste only ever edits an open text input; it is never
    /// interpreted as shortcuts and never submits anything.
    pub fn paste(&mut self, text: &str) {
        self.last_click = None;
        self.message_clock = None;
        if self.too_small || self.confirmation.is_some() || (self.mode == Mode::Normal && self.help)
        {
            return;
        }
        if self.mode != Mode::Normal {
            self.editor.paste(text);
            if self.mode == Mode::Search {
                self.search_edited();
            }
            self.message.clear();
        } else if let Some((selected, values)) = &mut self.filter_edit {
            if self.filter_input.text != values[*selected] {
                self.filter_input.set(&values[*selected]);
            }
            self.filter_input.paste(text);
            values[*selected] = self.filter_input.text.clone();
            self.facet_prefix = self.filter_input.text.clone();
            self.facet_due = Some(Instant::now() + Duration::from_millis(200));
            self.facet_index = None;
        } else {
            self.message = crate::t!("paste_ignored").into();
        }
    }
    /// Whether an auto-repeated key may act. Navigation, volume and text editing
    /// may repeat; anything that starts, stops, toggles, submits or deletes needs
    /// a fresh press.
    fn repeat_allowed(&self, key: &KeyEvent) -> bool {
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        let commits = matches!(
            key.code,
            KeyCode::Enter | KeyCode::Esc | KeyCode::Tab | KeyCode::BackTab
        );
        if self.confirmation.is_some() {
            false
        } else if self.mode != Mode::Normal {
            !commits
        } else if self.help {
            matches!(
                key.code,
                KeyCode::Char('j' | 'k')
                    | KeyCode::Up
                    | KeyCode::Down
                    | KeyCode::PageUp
                    | KeyCode::PageDown
            ) || control && matches!(key.code, KeyCode::Char('e' | 'y' | 'f' | 'b'))
        } else if self.filter_edit.is_some() {
            !commits && !(control && key.code == KeyCode::Char('u'))
        } else if control {
            matches!(
                key.code,
                KeyCode::Char('h' | 'j' | 'k' | 'l' | 'u' | 'd' | 'e' | 'y' | 'f' | 'b')
                    | KeyCode::Left
                    | KeyCode::Right
                    | KeyCode::Up
                    | KeyCode::Down
            )
        } else {
            matches!(
                key.code,
                KeyCode::Char('j' | 'k' | 'h' | 'l' | '+' | '=' | '-')
                    | KeyCode::Up
                    | KeyCode::Down
                    | KeyCode::Left
                    | KeyCode::Right
                    | KeyCode::PageUp
                    | KeyCode::PageDown
            )
        }
    }
    pub fn key(&mut self, key: KeyEvent, view: &View, sender: &CommandSender) {
        self.last_click = None;
        // A repeated identical notice must get a fresh lifetime.
        self.message_clock = None;
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c')
            || self.too_small && key.code == KeyCode::Char('q')
        {
            self.quit = true;
            return;
        }
        if key.kind == crossterm::event::KeyEventKind::Repeat && !self.repeat_allowed(&key) {
            return;
        }
        if self.too_small {
            if key.code == KeyCode::Esc {
                if self.mode == Mode::Search {
                    self.cancel_search();
                }
                self.mode = Mode::Normal;
                self.help = false;
                self.filter_edit = None;
                self.message.clear();
            }
            return;
        }
        if let Some(mut confirmation) = self.confirmation.take() {
            match key.code {
                KeyCode::Char('y') if key.kind == crossterm::event::KeyEventKind::Press => {
                    if self.send(sender, confirmation.command()) {
                        self.message = crate::t!("delete_submitted").into();
                    } else {
                        confirmation.error = self.message.clone();
                        self.confirmation = Some(confirmation);
                    }
                }
                KeyCode::Enter | KeyCode::Char('n' | 'q') | KeyCode::Esc => self.message.clear(),
                _ => self.confirmation = Some(confirmation),
            }
            return;
        }
        if self.mode != Mode::Normal {
            if key.code == KeyCode::Esc {
                if self.mode == Mode::Search {
                    self.cancel_search();
                }
                self.mode = Mode::Normal;
                self.message.clear();
                return;
            }
            if key.code == KeyCode::Enter {
                if self.mode == Mode::Command {
                    self.execute_line(view, sender);
                } else {
                    self.search_origin = None;
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
                    .recordings_for(&self.filter)
                    .iter()
                    .take(20)
                    .map(|r| r.id.clone())
                    .collect();
            }
            self.editor.key(key, self.mode == Mode::Command);
            if self.mode == Mode::Search {
                self.search_edited();
            }
            self.message.clear();
            return;
        }
        if self.help {
            let control = key.modifiers.contains(KeyModifiers::CONTROL);
            match key.code {
                KeyCode::Esc | KeyCode::Char('?') | KeyCode::Char('q') => self.help = false,
                KeyCode::Char('e') if control => self.move_popup(PopupMove::Lines(1)),
                KeyCode::Char('y') if control => self.move_popup(PopupMove::Lines(-1)),
                KeyCode::Char('f') if control => self.move_popup(PopupMove::Pages(1)),
                KeyCode::Char('b') if control => self.move_popup(PopupMove::Pages(-1)),
                KeyCode::Down | KeyCode::Char('j') => self.move_popup(PopupMove::Lines(1)),
                KeyCode::Up | KeyCode::Char('k') => self.move_popup(PopupMove::Lines(-1)),
                KeyCode::PageDown => self.move_popup(PopupMove::Pages(1)),
                KeyCode::PageUp => self.move_popup(PopupMove::Pages(-1)),
                KeyCode::Home | KeyCode::Char('g') => self.move_popup(PopupMove::Top),
                KeyCode::End | KeyCode::Char('G') => self.move_popup(PopupMove::Bottom),
                // Failure details and command usage replace the keyboard help
                // that `?` would otherwise open, so offer a way back to it.
                KeyCode::Char('h') if !control && self.help_content.is_some() => {
                    self.help_content = None;
                    self.help_title = None;
                    self.help_scroll = 0;
                }
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
                    let candidates = matching_facets(&view.facets[*selected], &self.facet_prefix);
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
                    self.facet_due = None;
                }
                KeyCode::BackTab => {
                    *selected = (*selected + 3) % 4;
                    self.facet_index = None;
                    self.filter_input.set(&values[*selected]);
                    self.facet_prefix = values[*selected].clone();
                    self.facet_due = None;
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
                    Err(_) => self.message = crate::t!("invalid_min_bitrate").into(),
                },
                _ => {
                    self.filter_input.key(key, false);
                    values[*selected] = self.filter_input.text.clone();
                    self.facet_prefix = self.filter_input.text.clone();
                    self.facet_due = Some(Instant::now() + Duration::from_millis(200));
                    self.facet_index = None;
                }
            }
            return;
        }
        if self.jump_key(key, view, sender) {
            return;
        }
        let previous_message = std::mem::take(&mut self.message);
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            match key.code {
                KeyCode::Char('c') => self.quit = true,
                KeyCode::Char('h') | KeyCode::Left => self.focus_direction(-1, 0),
                KeyCode::Char('l') | KeyCode::Right => self.focus_direction(1, 0),
                KeyCode::Char('j') | KeyCode::Down => self.focus_direction(0, 1),
                KeyCode::Char('k') | KeyCode::Up => self.focus_direction(0, -1),
                KeyCode::Char('u') => self.move_selection(-10, view),
                KeyCode::Char('d') => self.move_selection(10, view),
                KeyCode::Char('e') => self.move_selection(1, view),
                KeyCode::Char('y') => self.move_selection(-1, view),
                KeyCode::Char('f') => self.page(true, view, sender),
                KeyCode::Char('b') => self.page(false, view, sender),
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
                self.search_origin = Some(SearchOrigin {
                    filter: self.filter.clone(),
                    selected_id: self.selected_id.clone(),
                    index: self.list.selected().unwrap_or(0),
                    edited: false,
                });
                self.editor.begin(&self.filter.query);
                self.focus = Focus::List;
            }
            KeyCode::Char('?') => {
                let mut errors: Vec<(&'static str, String)> = vec![];
                if view.operation_error_id > self.dismissed_error {
                    errors.push((
                        crate::t!("operation_error_title"),
                        view.operation_error.clone(),
                    ));
                }
                if let crate::engine::PlaybackState::Failed { reason } = &view.status.playback_state
                {
                    errors.push((
                        crate::t!("playback_error_title"),
                        crate::t!("playback_failed", "reason" => reason),
                    ));
                }
                if let crate::engine::RecordingState::Failed { reason } =
                    &view.status.recording_state
                {
                    errors.push((
                        crate::t!("recording_error_title"),
                        crate::t!("recording_failed_detail", "reason" => reason),
                    ));
                }
                // A single failure names itself; several share the generic title.
                self.help_title = match errors.as_slice() {
                    [] => None,
                    [(title, _)] => Some(*title),
                    _ => Some(crate::t!("error_details_title")),
                };
                let mut details: Vec<String> =
                    errors.into_iter().map(|(_, detail)| detail).collect();
                if !details.is_empty() && !previous_message.is_empty() {
                    details.push(previous_message);
                }
                self.help_content = (!details.is_empty()).then(|| details.join("\n\n"));
                self.help = true;
                self.help_scroll = 0;
            }
            KeyCode::Char('q') => self.quit = true,
            KeyCode::Esc => self.dismissed_error = view.operation_error_id,
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
            KeyCode::PageDown => self.page(true, view, sender),
            KeyCode::PageUp => self.page(false, view, sender),
            KeyCode::Enter
                if self.focus == Focus::List && view.page_error(&self.filter).is_some() =>
            {
                self.help_content = view.page_error(&self.filter).map(str::to_owned);
                self.help_title = Some(crate::t!("list_error_title"));
                self.help = true;
                self.help_scroll = 0;
            }
            KeyCode::Enter if self.focus == Focus::Sources => self.focus = Focus::List,
            KeyCode::Enter if self.focus == Focus::Player => {
                if let Some(station) = &view.status.station {
                    self.request_info(station.stationuuid.clone(), sender);
                }
            }
            KeyCode::Enter if self.focus == Focus::List => {
                if self.source == Source::Recordings {
                    if let Some(r) = view
                        .recordings_for(&self.filter)
                        .get(self.list.selected().unwrap_or(0))
                    {
                        self.send(sender, Command::PlayRecording(r.id.clone()));
                    }
                } else if let Some(s) = self.selected(view) {
                    self.send(sender, Command::Play(s.stationuuid.clone()));
                }
            }
            KeyCode::Char('D') if self.source == Source::Recordings => {
                if let Some(r) = view
                    .recordings_for(&self.filter)
                    .get(self.list.selected().unwrap_or(0))
                {
                    self.confirmation = Confirmation::new(
                        Command::DeleteRecording {
                            id: r.id.clone(),
                            index_only: false,
                        },
                        view,
                    );
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
                } else {
                    self.message = crate::t!("no_station_selected").into();
                }
            }
            KeyCode::Char('f') => self.message = crate::t!("recordings_no_favorite").into(),
            KeyCode::Char('F') if self.source != Source::Recordings => {
                self.filter_input
                    .set(self.filter.country.as_deref().unwrap_or(""));
                self.facet_index = None;
                self.facet_prefix = self.filter_input.text.clone();
                self.facets_request = None;
                self.facet_due = None;
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
            KeyCode::Char('/') | KeyCode::Char('F') => {
                self.message = crate::t!("recordings_no_search").into();
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
                let whole = key.code == KeyCode::Char('Y');
                let copied = if self.source == Source::Recordings {
                    view.recordings_for(&self.filter)
                        .get(self.list.selected().unwrap_or(0))
                        .map(|r| {
                            let name = target_name(&r.station_name, &r.id);
                            if whole {
                                (
                                    r.path.clone(),
                                    crate::t!("clipboard_recording_path", "name" => name),
                                )
                            } else {
                                (
                                    r.id.clone(),
                                    crate::t!("clipboard_recording_id", "name" => name),
                                )
                            }
                        })
                } else {
                    self.selected(view).map(|s| {
                        let name = target_name(&s.name, &s.stationuuid);
                        if whole {
                            (
                                s.primary_url().to_owned(),
                                crate::t!("clipboard_station_url", "name" => name),
                            )
                        } else {
                            (
                                s.stationuuid.clone(),
                                crate::t!("clipboard_station_uuid", "name" => name),
                            )
                        }
                    })
                };
                match copied {
                    Some((value, notice)) => {
                        self.message = if super::output::clipboard_truncated(&value) {
                            crate::t!("clipboard_truncated", "notice" => notice)
                        } else {
                            notice
                        };
                        self.clipboard = Some(value);
                    }
                    None => {
                        self.clipboard = None;
                        self.message = if self.source == Source::Recordings {
                            crate::t!("no_recording_selected")
                        } else {
                            crate::t!("no_station_selected")
                        }
                        .into();
                    }
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
        self.mouse_at(event, view, sender, Instant::now());
    }
    pub(super) fn mouse_at(
        &mut self,
        event: MouseEvent,
        view: &View,
        sender: &CommandSender,
        now: Instant,
    ) {
        if self.too_small
            || !self.mouse_enabled
            || self.mode != Mode::Normal
            || self.confirmation.is_some()
        {
            self.last_click = None;
            return;
        }
        if self.help || self.filter_edit.is_some() {
            self.last_click = None;
            let code = match event.kind {
                MouseEventKind::ScrollDown => Some(KeyCode::Down),
                MouseEventKind::ScrollUp => Some(KeyCode::Up),
                _ => None,
            };
            if let Some(code) = code {
                self.key(KeyEvent::new(code, KeyModifiers::NONE), view, sender);
            }
            return;
        }
        let previous = match event.kind {
            MouseEventKind::Down(MouseButton::Left) => self.last_click.take(),
            MouseEventKind::Up(MouseButton::Left) => {
                if self
                    .last_click
                    .as_ref()
                    .is_some_and(|(_, x, y, _, _)| (*x, *y) != (event.column, event.row))
                {
                    self.last_click = None;
                }
                None
            }
            _ => {
                self.last_click = None;
                None
            }
        };
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
                            if let Some(id) = self.selection_id(view)
                                && event.column > rect.x
                                && event.column < rect.right().saturating_sub(1)
                            {
                                let uuid = format!("{}:{id}", self.source.preference());
                                let revision = if self.source == Source::Recordings {
                                    view.recordings_revision
                                } else {
                                    view.rows_revision
                                };
                                if previous.is_some_and(|(at, x, y, id, previous_revision)| {
                                    now.saturating_duration_since(at) <= Duration::from_millis(500)
                                        && (x, y) == (event.column, event.row)
                                        && id == uuid
                                        && previous_revision == revision
                                }) {
                                    self.send(
                                        sender,
                                        if self.source == Source::Recordings {
                                            Command::PlayRecording(id)
                                        } else {
                                            Command::Play(id)
                                        },
                                    );
                                } else {
                                    self.last_click =
                                        Some((now, event.column, event.row, uuid, revision));
                                }
                            }
                        }
                    }
                }
            }
            MouseEventKind::ScrollDown
                if matches!(focus, Focus::List | Focus::Details | Focus::Activity) =>
            {
                self.focus = focus;
                self.move_selection(3, view);
            }
            MouseEventKind::ScrollUp
                if matches!(focus, Focus::List | Focus::Details | Focus::Activity) =>
            {
                self.focus = focus;
                self.move_selection(-3, view);
            }
            _ => {}
        }
    }
}
/// A label for a copy or favorite target: its cleaned name, or its id when unnamed.
pub(super) fn target_name(name: &str, id: &str) -> String {
    let name = crate::clean_text(name);
    if name.trim().is_empty() {
        crate::clean_text(id)
    } else {
        name
    }
}

// Locating a failed station must not read like a successful playback jump.
fn located_notice(view: &View) -> String {
    if matches!(
        view.status.playback_state,
        crate::engine::PlaybackState::Failed { .. }
    ) {
        crate::t!("jump_failed").into()
    } else {
        String::new()
    }
}

fn nonempty(text: &str) -> Option<String> {
    let text = text.trim();
    (!text.is_empty()).then(|| text.into())
}

fn matching_facets<'a>(
    facets: &'a [crate::model::Facet],
    prefix: &str,
) -> Vec<&'a crate::model::Facet> {
    let prefix = prefix.trim().to_lowercase();
    facets
        .iter()
        .filter(|facet| facet.value.to_lowercase().starts_with(&prefix))
        .collect()
}
