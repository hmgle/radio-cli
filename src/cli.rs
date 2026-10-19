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
    pub lang: Option<String>,
    #[arg(long, global = true)]
    pub config: Option<PathBuf>,
    #[arg(long, global = true)]
    pub data_dir: Option<PathBuf>,
    #[arg(long, global = true)]
    pub api_url: Option<String>,
    #[arg(long, global = true)]
    pub json: bool,
    /// Emit one versioned success/error envelope (legacy --json is unchanged).
    #[arg(long, global = true, conflicts_with = "json")]
    pub json_v2: bool,
    #[command(subcommand)]
    pub command: Option<Action>,
}

#[derive(Args, Debug, Clone, Default)]
pub struct Search {
    #[arg(long, value_enum, default_value = "clickcount")]
    pub sort: crate::model::Sort,
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
            sort: self.sort,
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
            "{} / {total} {} · offset {} · {}",
            items.len(),
            crate::t!("cli_items"),
            page.offset,
            if has_more {
                crate::t!("cli_more")
            } else {
                crate::t!("cli_last")
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
    /// Generate shell completion code without opening the database.
    Completions {
        shell: clap_complete::Shell,
    },
    /// Generate the radio(1) manual page.
    Man,
    Countries(FacetArgs),
    Languages(FacetArgs),
    Tags(FacetArgs),
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

#[derive(Args)]
pub struct FacetArgs {
    #[arg(long, default_value = "")]
    pub prefix: String,
}
#[derive(Subcommand)]
pub enum Favorite {
    List(Page),
    Add { uuid: String },
    Remove { uuid: String },
}
#[derive(Subcommand)]
pub enum StationAction {
    List(Page),
    Add {
        name: String,
        url: String,
        #[arg(long)]
        backup_url: Vec<String>,
    },
    Edit {
        uuid: String,
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        url: Option<String>,
        #[arg(long)]
        backup_url: Vec<String>,
    },
    Remove {
        uuid: String,
        #[arg(long)]
        keep_references: bool,
    },
    Backups {
        uuid: String,
        #[arg(long)]
        add: Vec<String>,
        #[arg(long)]
        remove: Vec<String>,
    },
}
#[derive(Subcommand)]
pub enum RecordingAction {
    List(Page),
    Recover,
    Play {
        id: String,
    },
    Remove {
        id: String,
        /// Remove the index only, retaining the audio and sidecar files.
        #[arg(long)]
        index_only: bool,
    },
    Open,
}

fn emit(value: &impl serde::Serialize) -> Result<()> {
    if JSON_V2.get() {
        JSON_DATA.set(Some(serde_json::to_value(value)?));
        return Ok(());
    }
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
            if s.active {
                ""
            } else {
                crate::t!("cli_inactive")
            }
        );
    }
    Ok(())
}

pub fn run(cli: Cli) -> Result<()> {
    JSON_V2.set(cli.json_v2);
    JSON_DATA.set(None);
    let v2 = cli.json_v2;
    let result = run_inner(cli);
    if v2 && result.is_ok() {
        println!("{}", serde_json::json!({"ok":true,"data":JSON_DATA.take()}));
    }
    JSON_V2.set(false);
    result
}

thread_local! {
    static JSON_V2: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static JSON_DATA: std::cell::Cell<Option<serde_json::Value>> = const { std::cell::Cell::new(None) };
}

fn run_inner(mut cli: Cli) -> Result<()> {
    cli.json |= cli.json_v2;
    crate::messages::preview(cli.lang.as_deref());
    if let Some(Action::Completions { shell }) = cli.command.as_ref() {
        let mut bytes = Vec::new();
        clap_complete::generate(*shell, &mut Cli::command(), "radio", &mut bytes);
        return emit_generated(bytes, cli.json);
    }
    if matches!(cli.command, Some(Action::Man)) {
        let mut bytes = Vec::new();
        clap_mangen::Man::new(Cli::command()).render(&mut bytes)?;
        return emit_generated(bytes, cli.json);
    }
    if cli.json_v2 && matches!(cli.command, None | Some(Action::Tui)) {
        bail!(
            crate::error::DomainError::new(crate::error::ErrorCode::UsageError)
                .with("detail", "--json-v2 requires a noninteractive subcommand")
        );
    }
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
    crate::messages::init(cli.lang.as_deref(), config.settings.language.as_deref());
    config.prepare()?;
    let stop = Arc::new(AtomicBool::new(false));
    signal_hook::flag::register(signal_hook::consts::SIGINT, stop.clone())?;
    signal_hook::flag::register(signal_hook::consts::SIGTERM, stop.clone())?;
    let mut store = Store::open(&config.data_dir.join("radio.db"))?;
    match action {
        Action::Completions { .. } | Action::Man => unreachable!(),
        Action::Countries(ref args) | Action::Languages(ref args) | Action::Tags(ref args) => {
            let kind = match action {
                Action::Countries(_) => crate::model::FacetKind::Country,
                Action::Languages(_) => crate::model::FacetKind::Language,
                _ => crate::model::FacetKind::Tag,
            };
            let facets = store.facets(kind, &args.prefix)?;
            if cli.json {
                emit(&facets)?;
            } else {
                for facet in facets {
                    println!("{}\t{}", crate::clean_text(&facet.value), facet.count);
                }
            }
        }
        Action::Tui => {
            drop(store);
            crate::tui::run(config, stop)?;
        }
        Action::Doctor => doctor(&config, cli.json, &stop)?,
        Action::Sync { accept_shrink } => {
            let mut directory = Directory::new(&config, stop.clone())?;
            let count = directory.sync_with_policy(
                &mut store,
                |n| eprintln!("{}", crate::t!("cli_sync_progress", "count" => n)),
                accept_shrink,
            )?;
            if cli.json {
                emit(&serde_json::json!({"stations":count,"last_sync":store.meta("last_sync")?}))?;
            } else {
                println!("{}", crate::t!("cli_sync_done", "count" => count));
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
            Favorite::Add { ref uuid } | Favorite::Remove { ref uuid } => {
                let added = matches!(action, Favorite::Add { .. });
                let station = store.station(uuid)?;
                store.favorite(uuid, added)?;
                if cli.json {
                    emit(&serde_json::json!({
                        "stationuuid": uuid,
                        "name": station.name,
                        "favorite": added,
                    }))?;
                } else {
                    println!(
                        "{}：{}",
                        if added {
                            crate::t!("cli_favorited")
                        } else {
                            crate::t!("cli_unfavorited")
                        },
                        crate::clean_text(&station.name)
                    );
                }
            }
        },
        Action::Station {
            action:
                StationAction::Add {
                    name,
                    url,
                    backup_url,
                },
        } => {
            let station = crate::service::add_station(&mut store, name, url, &backup_url)?;
            if cli.json {
                emit(&station)?;
            } else {
                println!("{}", station.stationuuid);
            }
        }
        Action::Station {
            action: StationAction::List(page),
        } => list_stations(
            &store,
            Filter {
                custom: true,
                ..Filter::default()
            },
            &page,
            cli.json,
        )?,
        Action::Station {
            action:
                StationAction::Edit {
                    uuid,
                    name,
                    url,
                    backup_url,
                },
        } => {
            emit(&crate::service::edit_station(
                &mut store,
                &uuid,
                name,
                url,
                &backup_url,
            )?)?;
        }
        Action::Station {
            action:
                StationAction::Remove {
                    uuid,
                    keep_references,
                },
        } => {
            let placeholder_kept = store.remove_custom(&uuid, keep_references)?;
            emit(
                &serde_json::json!({"stationuuid":uuid,"removed":true,"placeholder_kept":placeholder_kept}),
            )?;
        }
        Action::Station {
            action: StationAction::Backups { uuid, add, remove },
        } => {
            emit(&store.update_backups(&uuid, &add, &remove)?)?;
        }
        Action::Recordings {
            action: RecordingAction::Play { id },
        } => {
            drop(store);
            let mut engine = Engine::new(config, stop.clone())?;
            engine.play_recording(&id)?;
            playback_loop(&mut engine, &stop)?;
        }
        Action::Recordings {
            action: RecordingAction::Remove { id, index_only },
        } => {
            let _lock = crate::process::MediaLock::acquire(&config.runtime_dir)?;
            crate::service::delete_recording(&store, &id, index_only)?;
            emit(&serde_json::json!({"id":id,"removed":true,"index_only":index_only}))?;
        }
        Action::Recordings {
            action: RecordingAction::Open,
        } => {
            crate::service::open_recordings(&config.recording_dir, &stop)?;
        }
        Action::Recordings {
            action: RecordingAction::Recover,
        } => {
            let _lock = crate::process::MediaLock::acquire(&config.runtime_dir)?;
            let warnings = recorder::recover(&store)?;
            if cli.json {
                emit(&serde_json::json!({"warnings": warnings}))?;
            } else if warnings.is_empty() {
                println!("{}", crate::t!("cli_recovered"));
            } else {
                for warning in &warnings {
                    eprintln!("{warning}");
                }
            }
            if !warnings.is_empty() {
                bail!("{}", crate::t!("cli_recovery_pending"));
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
                eprintln!("{}", crate::t!("cli_bootstrap"));
                engine.bootstrap()?;
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
            eprintln!("{}", crate::t!("cli_probe"));
            engine.standalone_record(&uuid, duration, output_dir.as_deref())?;
            eprintln!(
                "{} · Ctrl-C: {}",
                engine.status.message, engine.status.recording_path
            );
            while !stop.load(Ordering::Relaxed) && engine.status.recording() {
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
                bail!("{}", crate::t!("cli_recording_incomplete"));
            }
        }
    }
    Ok(())
}

fn emit_generated(bytes: Vec<u8>, json: bool) -> Result<()> {
    if json {
        emit(&String::from_utf8(bytes)?)
    } else {
        use std::io::Write;
        io::stdout().write_all(&bytes)?;
        Ok(())
    }
}

fn playback_loop(engine: &mut Engine, stop: &AtomicBool) -> Result<()> {
    let mut previous = String::new();
    while !stop.load(Ordering::Relaxed) && !engine.playback_failed && engine.playback_active() {
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
        let (ok, detail, code) = match result {
            Ok(output) => (
                true,
                String::from_utf8_lossy(&output)
                    .lines()
                    .next()
                    .unwrap_or("")
                    .to_owned(),
                None,
            ),
            Err(e) => (
                false,
                e.to_string(),
                e.downcast_ref::<crate::error::DomainError>()
                    .map(|e| e.code.as_str()),
            ),
        };
        failed |= !ok;
        checks.push(serde_json::json!({"name":name,"ok":ok,"detail":detail,"code":code}));
    }
    std::fs::create_dir_all(&config.recording_dir)
        .context(crate::t!("cli_recording_dir_unavailable"))?;
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
    let available = fs2::available_space(&config.recording_dir)?;
    let minimum = config
        .settings
        .recording_min_free_mb
        .saturating_mul(1024 * 1024);
    let enough = available >= minimum;
    failed |= !enough;
    checks.push(serde_json::json!({"name":"recording_space","ok":enough,
        "available_bytes":available,"minimum_bytes":minimum,
        "detail":format!("{available} bytes available; reserve {minimum} bytes")}));
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
        if let Some(check) = report["checks"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|check| check["code"] == "missing_dependency")
        {
            bail!(
                crate::error::DomainError::new(crate::error::ErrorCode::MissingDependency)
                    .with("program", check["name"].as_str().unwrap_or(""))
            );
        }
        bail!("{}", crate::t!("cli_doctor_failed"));
    }
    Ok(())
}
