use super::state::{Focus, Source};
use crate::{
    cli::{Action, Cli},
    model::{Filter, Station},
};
use anyhow::{Result, bail};
use clap::Parser;
use std::{path::PathBuf, time::Duration};

pub(super) enum Command {
    Search(Filter),
    Play(String),
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
    Sync,
    Cancel,
    Recordings(Filter),
    Info(String),
    Doctor,
}

pub(super) enum Parsed {
    Worker(Command),
    Search(Filter),
    Source(Source, crate::cli::Page),
    Focus(Focus),
    Help,
    Quit,
}

pub(super) const COMPLETIONS: &[&str] = &[
    "sync",
    "sync-cancel",
    "search",
    "play",
    "random",
    "pause",
    "stop",
    "volume",
    "record",
    "record stop",
    "recordings list",
    "favorites list",
    "favorites add",
    "favorites remove",
    "history",
    "info",
    "current",
    "station add",
    "doctor",
    "help",
    "quit",
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
        bail!("请输入命令；:help 查看帮助");
    };
    let selected_id = || {
        selected
            .map(|s| s.stationuuid.clone())
            .ok_or_else(|| anyhow::anyhow!("请先选择电台或指定 UUID"))
    };
    match name.as_str() {
        "current" if words.len() == 1 => {
            return Ok(Parsed::Worker(Command::Info(
                current
                    .ok_or_else(|| anyhow::anyhow!("尚未播放"))?
                    .stationuuid
                    .clone(),
            )));
        }
        "q" | "quit" if words.len() == 1 => return Ok(Parsed::Quit),
        "help" if words.len() == 1 => return Ok(Parsed::Help),
        "sync-cancel" if words.len() == 1 => return Ok(Parsed::Worker(Command::Cancel)),
        "pause" if words.len() == 1 => return Ok(Parsed::Worker(Command::Pause)),
        "stop" if words.len() == 1 => return Ok(Parsed::Worker(Command::Stop)),
        "volume" => {
            let value = words
                .get(1)
                .filter(|_| words.len() == 2)
                .and_then(|s| s.parse::<u8>().ok())
                .filter(|n| *n <= 100)
                .ok_or_else(|| anyhow::anyhow!("用法: volume <0–100>"))?;
            return Ok(Parsed::Worker(Command::Volume(value)));
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
                _ => bail!("用法: focus sources|list|details|player|activity"),
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
                .ok_or_else(|| anyhow::anyhow!("尚未播放；请指定电台 UUID"))?,
        );
    }
    let cli = Cli::try_parse_from(std::iter::once("radio".to_owned()).chain(words))
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    // Global process/config flags have no meaning inside an existing session.
    if cli.config.is_some() || cli.data_dir.is_some() || cli.api_url.is_some() || cli.json {
        bail!("TUI 命令不接受进程级配置参数");
    }
    let parsed = match cli.command {
        Some(Action::Sync) => Parsed::Worker(Command::Sync),
        Some(Action::Search(search)) => Parsed::Search(search.filter()),
        Some(Action::Play { uuid }) => Parsed::Worker(Command::Play(uuid)),
        Some(Action::Random(search)) => Parsed::Worker(Command::Random(search.filter())),
        Some(Action::Info { uuid }) => Parsed::Worker(Command::Info(uuid)),
        Some(Action::History(page)) => Parsed::Source(Source::History, page),
        Some(Action::Favorites { action }) => match action {
            crate::cli::Favorite::List(page) => Parsed::Source(Source::Favorites, page),
            crate::cli::Favorite::Add { uuid } => Parsed::Worker(Command::Favorite(uuid, true)),
            crate::cli::Favorite::Remove { uuid } => Parsed::Worker(Command::Favorite(uuid, false)),
        },
        Some(Action::Recordings {
            action: crate::cli::RecordingAction::List(page),
        }) => Parsed::Source(Source::Recordings, page),
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
        _ => bail!("未知 TUI 命令；:help 查看帮助"),
    };
    if matches!(&parsed, Parsed::Source(_, page) if page.all) {
        bail!("TUI 使用 PgUp/PgDn 分页；--all 仅供 CLI 导出");
    }
    Ok(parsed)
}

pub(super) const HELP: &str = "NORMAL 普通模式\n\
Tab / Shift-Tab  切换面板     1–5  直达面板\n\
Ctrl-h/j/k/l 或 Ctrl-方向键  按空间移动焦点\n\
[ / ] 切换目录分类     j/k、方向键  导航/滚动\n\
gg / G  首项/末项     Ctrl-u/d  移动十项\n\
Enter  播放列表选中项（来源面板中进入列表）\n\
Space 暂停  n 随机  f 收藏选中项  r 录音  +/- 音量\n\
/  搜索模式（Enter 确认，Esc 恢复原搜索）\n\
F  筛选对话框     ?  帮助     q 退出\n\n\
COMMAND 命令模式\n\
: 输入命令；Enter 执行；Esc 返回普通模式；Ctrl-c 收尾并退出\n\
Tab / Shift-Tab 补全；↑/↓ 历史；←/→ 编辑光标\n\
Home / Ctrl-a 行首；End / Ctrl-e 行尾；Ctrl-u 清空\n\n\
:sync                   后台同步目录（不停止播放）\n\
:sync-cancel            取消目录任务，保留旧目录\n\
:search jazz --country FR --min-bitrate 128\n\
:play [UUID]            播放选中电台或指定 UUID\n\
:random [搜索筛选参数]   不传参数时沿用当前筛选\n\
:info [UUID]            在详情面板查看电台\n\
:current                查看当前播放电台完整详情\n\
:favorites list|add|remove [UUID]\n\
:history                最近播放\n\
:recordings list        录音记录\n\
:station add '我的电台' https://example.com/live\n\
:record [UUID] [--duration 30m] [--output-dir 目录]\n\
:record stop            收尾录音\n\
:pause / :stop / :volume 70\n\
:doctor                 检查音频依赖与目录权限\n\
:focus sources|list|details|player|activity\n\
:quit / :q              收尾并退出\n\n\
命令只调用内置操作，不执行 shell。输入支持引号和中文。\n\
码率/编码来自目录，可能滞后；实际音频由播放器处理。\n\
Esc / ? 关闭帮助；j/k / PgUp/PgDn 滚动。";
