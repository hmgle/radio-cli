use super::state::{Focus, Source};
use crate::{
    cli::{Action, Cli},
    model::{Filter, Station},
};
use anyhow::{Result, bail};
use clap::{CommandFactory, Parser};
use std::{path::PathBuf, time::Duration};

use std::sync::mpsc::{self, Receiver, Sender, SyncSender};
use std::time::Instant;

pub(super) struct CommandSender {
    normal: SyncSender<Command>,
    control: Sender<Command>,
}
pub(super) struct CommandReceiver {
    normal: Receiver<Command>,
    control: Receiver<Command>,
}
fn priority(command: &Command) -> bool {
    matches!(
        command,
        Command::Pause
            | Command::Mute
            | Command::Stop
            | Command::RecordStop
            | Command::Volume(_)
            | Command::AdjustVolume(_)
            | Command::Cancel
    )
}
pub(super) fn channel(capacity: usize) -> (CommandSender, CommandReceiver) {
    let (normal, normal_rx) = mpsc::sync_channel(capacity);
    let (control, control_rx) = mpsc::channel();
    (
        CommandSender { normal, control },
        CommandReceiver {
            normal: normal_rx,
            control: control_rx,
        },
    )
}
impl CommandSender {
    pub fn try_send(&self, command: Command) -> Result<(), String> {
        if priority(&command) {
            self.control.send(command).map_err(|e| e.to_string())
        } else {
            self.normal.try_send(command).map_err(|e| e.to_string())
        }
    }
    #[cfg(test)]
    pub fn send(&self, command: Command) -> Result<(), String> {
        if priority(&command) {
            self.control.send(command).map_err(|e| e.to_string())
        } else {
            self.normal.send(command).map_err(|e| e.to_string())
        }
    }
}
impl CommandReceiver {
    #[cfg(test)]
    pub fn try_recv(&self) -> Result<Command, mpsc::TryRecvError> {
        self.control.try_recv().or_else(|_| self.normal.try_recv())
    }
    pub fn recv_timeout(&self, timeout: Duration) -> Result<Command, mpsc::RecvTimeoutError> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Ok(command) = self.control.try_recv() {
                return Ok(command);
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            match self
                .normal
                .recv_timeout(remaining.min(Duration::from_millis(10)))
            {
                Ok(command) => return Ok(command),
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return self
                        .control
                        .try_recv()
                        .map_err(|_| mpsc::RecvTimeoutError::Disconnected);
                }
                Err(_) if Instant::now() >= deadline => {
                    return Err(mpsc::RecvTimeoutError::Timeout);
                }
                Err(_) => {}
            }
        }
    }
    #[cfg(test)]
    pub fn recv(&self) -> Result<Command, mpsc::RecvTimeoutError> {
        self.recv_timeout(Duration::from_secs(2))
    }
    #[cfg(test)]
    pub fn try_iter(&self) -> impl Iterator<Item = Command> + '_ {
        std::iter::from_fn(|| self.try_recv().ok())
    }
}

#[derive(Clone)]
pub(super) enum Command {
    Subtitles(String),
    SubtitleModel(String),
    LocatePlaying {
        uuid: String,
        request: u64,
        limit: usize,
    },
    Mute,
    SetLanguage(String),
    Facets {
        selected: usize,
        prefix: String,
    },
    Backups(String),
    Station(crate::cli::StationAction),
    Search(Filter),
    Play(String),
    PlayRecording(String),
    DeleteRecording {
        id: String,
        index_only: bool,
    },
    OpenRecordings,
    Random(Filter),
    Favorite(String, bool),
    ToggleFavorite(String),
    Pause,
    Stop,
    Volume(u8),
    AdjustVolume(i32),
    RecordToggle,
    RecordStop,
    Record {
        uuid: Option<String>,
        duration: Option<Duration>,
        dir: Option<PathBuf>,
    },
    Add {
        name: String,
        url: String,
        backups: Vec<String>,
    },
    Sync(bool),
    Cancel,
    Recordings(Filter),
    Info(String, u64),
    Refresh,
    Doctor,
}

pub(super) enum Parsed {
    SubtitleHistory,
    Mouse(bool),
    HelpText(String),
    Worker(Command),
    Search(Filter),
    Source(Source, crate::cli::Page),
    Focus(Focus),
    Help,
    Quit,
}

pub(super) const COMPLETIONS: &[&str] = &[
    "subtitles on",
    "subtitles off",
    "subtitles toggle",
    "subtitles history",
    "subtitles clear",
    "subtitles retry",
    "set subtitles.model sensevoice-small-int8",
    "set subtitles.model sensevoice-small-yue-int8",
    "set subtitles.model qwen3-asr-0.6b-int8",
    "sync",
    "refresh",
    "sync-cancel",
    "search",
    "play",
    "random",
    "pause",
    "mute",
    "set language en",
    "set language zh-CN",
    "set mouse true",
    "set mouse false",
    "stop",
    "volume",
    "record",
    "record stop",
    "recordings list",
    "recordings play",
    "recordings remove",
    "open recordings",
    "favorites list",
    "favorites add",
    "favorites remove",
    "history",
    "explore",
    "info",
    "current",
    "station add",
    "station list",
    "station edit",
    "station remove",
    "station backups",
    "doctor",
    "help",
    "quit",
    "focus map",
    "focus sources",
    "focus list",
    "focus details",
    "focus player",
    "focus activity",
];

pub(super) fn parse(
    line: &str,
    selected: Option<&Station>,
    current: Option<&Station>,
    filter: &Filter,
) -> Result<Parsed> {
    let mut words = shell_words::split(line)?;
    let Some(name) = words.first().cloned() else {
        bail!(crate::t!("command_empty"));
    };
    let selected_id = || {
        selected
            .map(|s| s.stationuuid.clone())
            .ok_or_else(|| anyhow::anyhow!(crate::t!("command_need_station")))
    };
    match name.as_str() {
        "subtitles" => {
            return match words.get(1).map(String::as_str) {
                Some("history") if words.len() == 2 => Ok(Parsed::SubtitleHistory),
                Some("on" | "off" | "toggle" | "clear" | "retry") if words.len() == 2 => {
                    Ok(Parsed::Worker(Command::Subtitles(words[1].clone())))
                }
                _ => bail!(crate::t!("subtitle_usage")),
            };
        }
        "refresh" if words.len() == 1 => return Ok(Parsed::Worker(Command::Refresh)),
        "open" if words.get(1).map(String::as_str) == Some("recordings") && words.len() == 2 => {
            return Ok(Parsed::Worker(Command::OpenRecordings));
        }
        "current" if words.len() == 1 => {
            return Ok(Parsed::Worker(Command::Info(
                current
                    .ok_or_else(|| anyhow::anyhow!(crate::t!("command_nothing_playing")))?
                    .stationuuid
                    .clone(),
                0,
            )));
        }
        "q" | "quit" if words.len() == 1 => return Ok(Parsed::Quit),
        "help" if words.len() == 1 => return Ok(Parsed::Help),
        "help" => {
            let mut root = Cli::command();
            let mut selected = &mut root;
            for word in &words[1..] {
                selected = selected.find_subcommand_mut(word).ok_or_else(|| {
                    anyhow::anyhow!(crate::t!("command_unknown_help", "word" => word))
                })?;
            }
            return Ok(Parsed::HelpText(selected.render_long_help().to_string()));
        }
        "set" if words.len() == 3 => match words[1].as_str() {
            "subtitles.model" => {
                return Ok(Parsed::Worker(Command::SubtitleModel(words[2].clone())));
            }
            "language" if matches!(words[2].as_str(), "en" | "zh-CN") => {
                return Ok(Parsed::Worker(Command::SetLanguage(words[2].clone())));
            }
            "mouse" if matches!(words[2].as_str(), "true" | "false") => {
                return Ok(Parsed::Mouse(words[2] == "true"));
            }
            _ => bail!(crate::t!("command_usage_set")),
        },
        "sync-cancel" if words.len() == 1 => return Ok(Parsed::Worker(Command::Cancel)),
        "pause" if words.len() == 1 => return Ok(Parsed::Worker(Command::Pause)),
        "mute" if words.len() == 1 => return Ok(Parsed::Worker(Command::Mute)),
        "stop" if words.len() == 1 => return Ok(Parsed::Worker(Command::Stop)),
        "volume" => {
            let value = words
                .get(1)
                .filter(|_| words.len() == 2)
                .and_then(|s| s.parse::<u8>().ok())
                .filter(|n| *n <= 100)
                .ok_or_else(|| anyhow::anyhow!(crate::t!("command_usage_volume")))?;
            return Ok(Parsed::Worker(Command::Volume(value)));
        }
        "explore" if words.len() == 1 => {
            return Ok(Parsed::Source(
                Source::Explore,
                crate::cli::Page {
                    limit: 100,
                    offset: 0,
                    all: false,
                },
            ));
        }
        "focus" => {
            let panel = match words
                .get(1)
                .map(String::as_str)
                .filter(|_| words.len() == 2)
            {
                Some("sources" | "1") => Focus::Sources,
                Some("list" | "2") => Focus::List,
                Some("details" | "3") => Focus::Details,
                Some("player" | "4") => Focus::Player,
                Some("activity" | "5") => Focus::Activity,
                Some("map" | "6") => Focus::Map,
                _ => bail!(crate::t!("command_usage_focus")),
            };
            return Ok(Parsed::Focus(panel));
        }
        "record" if words.get(1).is_some_and(|s| s == "stop") && words.len() == 2 => {
            return Ok(Parsed::Worker(Command::RecordStop));
        }
        "random" if words.len() == 1 => return Ok(Parsed::Worker(Command::Random(filter.clone()))),
        "play" | "info" if words.len() == 1 => words.push(selected_id()?),
        "favorites" if words.len() == 2 && matches!(words[1].as_str(), "add" | "remove") => {
            words.push(selected_id()?)
        }
        _ => {}
    }
    // Record without UUID targets the actual playing source, including an active backup.
    let current_record =
        name == "record" && (words.len() == 1 || words.get(1).is_some_and(|s| s.starts_with('-')));
    if current_record {
        words.insert(
            1,
            current
                .map(|s| s.stationuuid.clone())
                .ok_or_else(|| anyhow::anyhow!(crate::t!("command_nothing_playing_uuid")))?,
        );
    }
    let cli = Cli::try_parse_from(std::iter::once("radio".to_owned()).chain(words))
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    // Global process/config flags have no meaning inside an existing session.
    if cli.config.is_some()
        || cli.lang.is_some()
        || cli.data_dir.is_some()
        || cli.api_url.is_some()
        || cli.json
        || cli.json_v2
    {
        bail!(crate::t!("command_process_options"));
    }
    let parsed = match cli.command {
        Some(Action::Sync { accept_shrink }) => Parsed::Worker(Command::Sync(accept_shrink)),
        Some(Action::Search(search)) => Parsed::Search(search.filter()),
        Some(Action::Play { uuid }) => Parsed::Worker(Command::Play(uuid)),
        Some(Action::Random(search)) => Parsed::Worker(Command::Random(search.filter())),
        Some(Action::Info { uuid }) => Parsed::Worker(Command::Info(uuid, 0)),
        Some(Action::History(page)) => Parsed::Source(Source::History, page),
        Some(Action::Favorites { action }) => match action {
            crate::cli::Favorite::List(page) => Parsed::Source(Source::Favorites, page),
            crate::cli::Favorite::Add { uuid } => Parsed::Worker(Command::Favorite(uuid, true)),
            crate::cli::Favorite::Remove { uuid } => Parsed::Worker(Command::Favorite(uuid, false)),
        },
        Some(Action::Recordings {
            action: crate::cli::RecordingAction::List(page),
        }) => Parsed::Source(Source::Recordings, page),
        Some(Action::Recordings {
            action: crate::cli::RecordingAction::Play { id },
        }) => Parsed::Worker(Command::PlayRecording(id)),
        Some(Action::Recordings {
            action: crate::cli::RecordingAction::Remove { id, index_only },
        }) => Parsed::Worker(Command::DeleteRecording { id, index_only }),
        Some(Action::Recordings {
            action: crate::cli::RecordingAction::Open,
        }) => Parsed::Worker(Command::OpenRecordings),
        Some(Action::Record {
            uuid,
            duration,
            output_dir,
        }) => Parsed::Worker(Command::Record {
            uuid: (!current_record).then_some(uuid),
            duration,
            dir: output_dir,
        }),
        Some(Action::Station {
            action:
                crate::cli::StationAction::Add {
                    name,
                    url,
                    backup_url,
                },
        }) => Parsed::Worker(Command::Add {
            name,
            url,
            backups: backup_url,
        }),
        Some(Action::Doctor) => Parsed::Worker(Command::Doctor),
        Some(Action::Station {
            action: crate::cli::StationAction::List(page),
        }) => {
            if page.all {
                bail!(crate::t!("command_paging"));
            }
            Parsed::Search(Filter {
                custom: true,
                limit: usize::from(page.limit),
                offset: page.offset,
                ..Filter::default()
            })
        }
        Some(Action::Station { action }) => Parsed::Worker(Command::Station(action)),
        _ => bail!(crate::t!("command_unknown")),
    };
    if matches!(&parsed, Parsed::Source(_, page) if page.all) {
        bail!(crate::t!("command_paging"));
    }
    Ok(parsed)
}

pub(super) fn completion_candidates(
    text: &str,
    stations: &[String],
    recordings: &[String],
) -> Vec<String> {
    let mut result: Vec<String> = COMPLETIONS
        .iter()
        .filter(|s| s.starts_with(text))
        .map(|s| (*s).into())
        .collect();
    let Ok(words) = shell_words::split(text) else {
        return result;
    };
    let Some(command) = words.first().map(String::as_str) else {
        return result;
    };
    let start = text.rfind(char::is_whitespace).map_or(0, |i| i + 1);
    let base = &text[..start];
    let prefix = &text[start..];
    let mut values: Vec<String> = vec![];
    let preceding = if prefix.is_empty() {
        words.last()
    } else {
        words.iter().rev().nth(1)
    }
    .map(String::as_str);
    if preceding == Some("--duration") {
        values.extend(["10m", "30m", "1h"].map(String::from));
    } else if preceding == Some("--sort") {
        values.extend(["name", "bitrate", "votes", "clickcount", "random"].map(String::from));
    } else {
        let options: &[&str] = match command {
            "record" => &["--duration", "--output-dir"],
            "search" | "random" => &[
                "--country",
                "--language",
                "--tag",
                "--min-bitrate",
                "--limit",
                "--offset",
                "--sort",
            ],
            "station" if words.get(1).map(String::as_str) == Some("add") => &["--backup-url"],
            "station" if words.get(1).map(String::as_str) == Some("edit") => {
                &["--name", "--url", "--backup-url"]
            }
            "station" if words.get(1).map(String::as_str) == Some("remove") => {
                &["--keep-references"]
            }
            "station" if words.get(1).map(String::as_str) == Some("backups") => {
                &["--add", "--remove"]
            }
            "recordings" if words.get(1).map(String::as_str) == Some("remove") => &["--index-only"],
            "history" | "favorites" | "recordings" => &["--limit", "--offset"],
            _ => &[],
        };
        values.extend(options.iter().map(|s| (*s).into()));
        if matches!(command, "record" | "play" | "info")
            && words.len() <= if prefix.is_empty() { 1 } else { 2 }
        {
            values.extend_from_slice(stations);
        }
        if command == "recordings"
            && matches!(words.get(1).map(String::as_str), Some("play" | "remove"))
        {
            values.extend_from_slice(recordings);
        }
        if text == "station add " {
            values.push("'Station name' https://example.com/live".into());
        }
    }
    result.extend(
        values
            .into_iter()
            .filter(|value| value.starts_with(prefix))
            .map(|value| format!("{base}{value}")),
    );
    let mut unique = std::collections::HashSet::new();
    result.retain(|candidate| unique.insert(candidate.clone()));
    result
}
