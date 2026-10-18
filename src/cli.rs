use crate::{
    config::Config,
    directory::Directory,
    engine::Engine,
    model::{Filter, Station},
    recorder,
    store::Store,
};
use anyhow::{Context, Result, bail};
use clap::{Args, CommandFactory, Parser, Subcommand};
use std::{
    io::{self, IsTerminal},
    path::PathBuf,
    process::Command,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};

#[derive(Parser)]
#[command(
    name = "radio",
    version,
    about = "轻量终端收音机：搜索、收藏、播放与本地录音"
)]
pub struct Cli {
    #[arg(long, global = true)]
    pub config: Option<PathBuf>,
    #[arg(long, global = true)]
    pub data_dir: Option<PathBuf>,
    #[arg(long, global = true)]
    pub api_url: Option<String>,
    #[arg(long, global = true)]
    pub json: bool,
    #[command(subcommand)]
    pub command: Option<Action>,
}

#[derive(Args, Debug, Clone, Default)]
pub struct Search {
    pub query: Option<String>,
    #[arg(long)]
    pub country: Option<String>,
    #[arg(long)]
    pub language: Option<String>,
    #[arg(long)]
    pub tag: Option<String>,
    #[arg(long, default_value_t = 0)]
    pub min_bitrate: u32,
    #[arg(long, default_value_t=100, value_parser=clap::value_parser!(u16).range(1..=1000))]
    pub limit: u16,
    #[arg(long, default_value_t = 0)]
    pub offset: usize,
}
impl Search {
    pub fn filter(&self) -> Filter {
        Filter {
            query: self.query.clone().unwrap_or_default(),
            country: self.country.clone(),
            language: self.language.clone(),
            tag: self.tag.clone(),
            min_bitrate: self.min_bitrate,
            limit: usize::from(self.limit),
            offset: self.offset,
            ..Filter::default()
        }
    }
}

#[derive(Args, Debug, Clone)]
pub struct Page {
    #[arg(long, default_value_t=100, value_parser=clap::value_parser!(u16).range(1..=1000))]
    pub limit: u16,
    #[arg(long, default_value_t = 0)]
    pub offset: usize,
    /// Export every matching entry from offset onward.
    #[arg(long)]
    pub all: bool,
}

fn emit_page<T: serde::Serialize>(
    items: &[T],
    total: usize,
    page: &Page,
    json: bool,
) -> Result<()> {
    let has_more = page.offset.saturating_add(items.len()) < total;
    if json {
        emit(
            &serde_json::json!({"items":items,"total":total,"offset":page.offset,"limit": if page.all { None } else { Some(page.limit) },"has_more":has_more}),
        )?;
    } else {
        eprintln!(
            "{} / {total} 项 · offset {}{}",
            items.len(),
            page.offset,
            if has_more {
                " · 后续可用 --offset / --all"
            } else {
                " · 最后一页"
            }
        );
    }
    Ok(())
}

fn list_stations(store: &Store, mut filter: Filter, page: &Page, json: bool) -> Result<()> {
    let total = store.search_count(&filter)?;
    filter.limit = usize::from(page.limit);
    filter.offset = page.offset;
    let mut rows = store.search(&filter)?;
    while page.all && page.offset.saturating_add(rows.len()) < total {
        filter.offset = page.offset.saturating_add(rows.len());
        let next = store.search(&filter)?;
        if next.is_empty() {
            break;
        }
        rows.extend(next);
    }
    if !json {
        print_stations(&rows, false)?;
    }
    emit_page(&rows, total, page, json)
}

#[derive(Subcommand)]
pub enum Action {
    Tui,
    Sync {
        #[arg(long)]
        accept_shrink: bool,
    },
    Search(Search),
    Info {
        uuid: String,
    },
    Play {
        uuid: String,
    },
    Random(Search),
    Favorites {
        #[command(subcommand)]
        action: Favorite,
    },
    History(Page),
    Station {
        #[command(subcommand)]
        action: StationAction,
    },
    Record {
        uuid: String,
        #[arg(long, value_parser=recorder::parse_duration)]
        duration: Option<Duration>,
        #[arg(long)]
        output_dir: Option<PathBuf>,
    },
    Recordings {
        #[command(subcommand)]
        action: RecordingAction,
    },
    Doctor,
}
#[derive(Subcommand)]
pub enum Favorite {
    List(Page),
    Add { uuid: String },
    Remove { uuid: String },
}
#[derive(Subcommand)]
pub enum StationAction {
    Add {
        name: String,
        url: String,
        #[arg(long)]
        backup_url: Vec<String>,
    },
}
#[derive(Subcommand)]
pub enum RecordingAction {
    List(Page),
    Recover,
}

fn emit(value: &impl serde::Serialize) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}
fn print_stations(rows: &[Station], json: bool) -> Result<()> {
    if json {
        return emit(&rows);
    }
    for s in rows {
        println!(
            "{}  {}{} [{}] {} kbps{}",
            s.stationuuid,
            if s.favorite { "★ " } else { "" },
            crate::clean_text(&s.name),
            crate::clean_text(&s.countrycode),
            s.bitrate,
            if s.active { "" } else { " [目录已失效]" }
        );
    }
    Ok(())
}

pub fn run(cli: Cli) -> Result<()> {
    let action = match cli.command {
        Some(action) => action,
        None if io::stdin().is_terminal() && io::stdout().is_terminal() => Action::Tui,
        None => {
            Cli::command().print_help()?;
            println!();
            return Ok(());
        }
    };
    let config = Config::load(
        cli.config.as_deref(),
        cli.data_dir.as_deref(),
        cli.api_url.as_deref(),
    )?;
    config.prepare()?;
    let stop = Arc::new(AtomicBool::new(false));
    signal_hook::flag::register(signal_hook::consts::SIGINT, stop.clone())?;
    signal_hook::flag::register(signal_hook::consts::SIGTERM, stop.clone())?;
    let mut store = Store::open(&config.data_dir.join("radio.db"))?;
    match action {
        Action::Tui => {
            drop(store);
            crate::tui::run(config, stop)?;
        }
        Action::Doctor => doctor(&config, cli.json, &stop)?,
        Action::Sync { accept_shrink } => {
            let mut directory = Directory::new(&config, stop.clone())?;
            let count = directory.sync_with_policy(
                &mut store,
                |n| eprintln!("已获取 {n} 台…"),
                accept_shrink,
            )?;
            if cli.json {
                emit(&serde_json::json!({"stations":count,"last_sync":store.meta("last_sync")?}))?;
            } else {
                println!("同步完成：{count} 台");
            }
        }
        Action::Search(search) => print_stations(&store.search(&search.filter())?, cli.json)?,
        Action::Info { uuid } => emit(&store.station(&uuid)?)?,
        Action::History(page) => list_stations(
            &store,
            Filter {
                history: true,
                ..Filter::default()
            },
            &page,
            cli.json,
        )?,
        Action::Favorites { action } => match action {
            Favorite::List(page) => list_stations(
                &store,
                Filter {
                    favorites: true,
                    ..Filter::default()
                },
                &page,
                cli.json,
            )?,
            Favorite::Add { uuid } => store.favorite(&uuid, true)?,
            Favorite::Remove { uuid } => store.favorite(&uuid, false)?,
        },
        Action::Station {
            action:
                StationAction::Add {
                    name,
                    url,
                    backup_url,
                },
        } => {
            crate::config::validate_url(&url)?;
            if name.trim().is_empty() {
                bail!("电台名称不能为空");
            }
            for backup in &backup_url {
                crate::config::validate_url(backup)?;
            }
            let station = Station {
                stationuuid: uuid::Uuid::new_v4().to_string(),
                name,
                url: url.clone(),
                url_resolved: url,
                active: true,
                custom: true,
                lastcheckok: 1,
                ..Station::default()
            };
            store.upsert(&station)?;
            for backup in backup_url {
                store.add_backup(&station.stationuuid, &backup)?;
            }
            if cli.json {
                emit(&station)?;
            } else {
                println!("{}", station.stationuuid);
            }
        }
        Action::Recordings {
            action: RecordingAction::Recover,
        } => {
            let _lock = crate::process::MediaLock::acquire(&config.runtime_dir)?;
            let warnings = recorder::recover(&store)?;
            if cli.json {
                emit(&serde_json::json!({"warnings": warnings}))?;
            } else if warnings.is_empty() {
                println!("恢复完成");
            } else {
                for warning in &warnings {
                    eprintln!("{warning}");
                }
            }
            if !warnings.is_empty() {
                bail!("部分录音仍需恢复，详见 recordings list");
            }
        }
        Action::Recordings {
            action: RecordingAction::List(page),
        } => {
            let total = store.recordings_count()?;
            let mut records = store.recordings_page(usize::from(page.limit), page.offset)?;
            while page.all && page.offset.saturating_add(records.len()) < total {
                let next = store.recordings_page(
                    usize::from(page.limit),
                    page.offset.saturating_add(records.len()),
                )?;
                if next.is_empty() {
                    break;
                }
                records.extend(next);
            }
            emit_page(&records, total, &page, cli.json)?;
            if !cli.json {
                for r in records {
                    println!(
                        "{} {} {} {}",
                        r.id,
                        r.status,
                        crate::clean_text(&r.station_name),
                        crate::clean_text(&format!(
                            "{} {}",
                            r.path,
                            r.error.as_deref().unwrap_or("")
                        ))
                    );
                }
            }
        }
        Action::Play { uuid } => {
            drop(store);
            let mut engine = Engine::new(config, stop.clone())?;
            engine.play(&uuid)?;
            playback_loop(&mut engine, &stop)?;
        }
        Action::Random(search) => {
            drop(store);
            let mut engine = Engine::new(config, stop.clone())?;
            if engine.store.count()? == 0 {
                eprintln!("获取热门电台…");
                engine.directory.bootstrap(&engine.store)?;
            }
            engine.random(search.filter())?;
            playback_loop(&mut engine, &stop)?;
        }
        Action::Record {
            uuid,
            duration,
            output_dir,
        } => {
            drop(store);
            let mut engine = Engine::new(config, stop.clone())?;
            eprintln!("检测音频编码…");
            engine.standalone_record(&uuid, duration, output_dir.as_deref())?;
            eprintln!("录音中，Ctrl-C 停止: {}", engine.status.recording_path);
            while !stop.load(Ordering::Relaxed) && engine.status.recording {
                engine.tick()?;
                thread::sleep(Duration::from_millis(100));
            }
            engine.shutdown()?;
            if cli.json {
                emit(&engine.store.recordings(1)?)?;
            } else {
                println!("{}", crate::clean_text(&engine.status.message));
            }
            if engine
                .store
                .recordings(1)?
                .first()
                .is_some_and(|r| r.status != "complete")
            {
                bail!("录音未完整保存，见 recordings list");
            }
        }
    }
    Ok(())
}

fn playback_loop(engine: &mut Engine, stop: &AtomicBool) -> Result<()> {
    let mut previous = String::new();
    while !stop.load(Ordering::Relaxed) && !engine.playback_failed {
        engine.tick()?;
        let line = format!(
            "{} | {} | {}",
            engine
                .status
                .station
                .as_ref()
                .map(|s| crate::clean_text(&s.name))
                .unwrap_or_default(),
            engine.status.title,
            engine.status.message
        );
        if line != previous {
            eprintln!("{line}");
            previous = line;
        }
        thread::sleep(Duration::from_millis(100));
    }
    engine.shutdown()?;
    if engine.playback_failed {
        bail!("{}", engine.status.message);
    }
    Ok(())
}

pub(crate) fn doctor_report(
    config: &Config,
    stop: &Arc<AtomicBool>,
) -> Result<(serde_json::Value, bool)> {
    let mut checks = vec![];
    let mut failed = false;
    for (name, exe, flag) in [
        ("mpv", &config.settings.mpv, "--version"),
        ("ffmpeg", &config.settings.ffmpeg, "-version"),
        ("ffprobe", &config.settings.ffprobe, "-version"),
    ] {
        let result =
            crate::process::output(Command::new(exe).arg(flag), Duration::from_secs(5), stop);
        let (ok, detail) = match result {
            Ok(output) => (
                true,
                String::from_utf8_lossy(&output)
                    .lines()
                    .next()
                    .unwrap_or("")
                    .to_owned(),
            ),
            Err(e) => (false, e.to_string()),
        };
        failed |= !ok;
        checks.push(serde_json::json!({"name":name,"ok":ok,"detail":detail}));
    }
    std::fs::create_dir_all(&config.recording_dir).context("录音目录不可用")?;
    let test_path = config
        .recording_dir
        .join(format!(".doctor-{}", uuid::Uuid::new_v4()));
    let writable = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&test_path);
    let writable_ok = writable.is_ok();
    failed |= !writable_ok;
    drop(writable);
    let _ = std::fs::remove_file(&test_path);
    checks.push(
        serde_json::json!({"name":"recording_dir","ok":writable_ok,"detail":config.recording_dir}),
    );
    let report = serde_json::json!({"checks":checks,"config":config.config_path,"database":config.data_dir.join("radio.db")});
    Ok((report, failed))
}

fn doctor(config: &Config, json: bool, stop: &Arc<AtomicBool>) -> Result<()> {
    let (report, failed) = doctor_report(config, stop)?;
    if json {
        emit(&report)?;
    } else {
        for check in report["checks"].as_array().into_iter().flatten() {
            println!(
                "{} {}: {}",
                if check["ok"] == true { "OK" } else { "FAIL" },
                check["name"].as_str().unwrap_or(""),
                crate::clean_text(check["detail"].as_str().unwrap_or(""))
            );
        }
    }
    if failed {
        bail!("依赖或目录检查未通过");
    }
    Ok(())
}
