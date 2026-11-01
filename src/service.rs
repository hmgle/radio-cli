use crate::{
    config::validate_url,
    error::{DomainError, ErrorCode},
    model::Station,
    store::Store,
};
use anyhow::{Result, bail};

mod diagnostics;
pub(crate) use diagnostics::doctor_report;

#[derive(Clone)]
pub enum StationCommand {
    Add {
        name: String,
        url: String,
        backups: Vec<String>,
    },
    Edit {
        uuid: String,
        name: Option<String>,
        url: Option<String>,
        backups: Vec<String>,
    },
    Remove {
        uuid: String,
        keep_references: bool,
    },
    Backups {
        uuid: String,
        add: Vec<String>,
        remove: Vec<String>,
    },
}

pub enum StationChange {
    Added(Station),
    Updated(Station),
    Removed {
        uuid: String,
        placeholder_kept: bool,
    },
    Backups {
        uuid: String,
        urls: Vec<String>,
    },
}

pub fn change_station(store: &mut Store, command: StationCommand) -> Result<StationChange> {
    Ok(match command {
        StationCommand::Add { name, url, backups } => {
            StationChange::Added(add_station(store, name, url, &backups)?)
        }
        StationCommand::Edit {
            uuid,
            name,
            url,
            backups,
        } => StationChange::Updated(edit_station(store, &uuid, name, url, &backups)?),
        StationCommand::Remove {
            uuid,
            keep_references,
        } => {
            let placeholder_kept = store.remove_custom(&uuid, keep_references)?;
            StationChange::Removed {
                uuid,
                placeholder_kept,
            }
        }
        StationCommand::Backups { uuid, add, remove } => StationChange::Backups {
            urls: store.update_backups(&uuid, &add, &remove)?,
            uuid,
        },
    })
}

pub fn normalized_url(url: &str) -> Result<String> {
    validate_url(url)?;
    let mut parsed = reqwest::Url::parse(url)?;
    parsed.set_fragment(None);
    Ok(parsed.into())
}

pub fn add_station(
    store: &mut Store,
    name: String,
    url: String,
    backups: &[String],
) -> Result<Station> {
    validate(&name, &url, backups)?;
    let station = Station {
        stationuuid: uuid::Uuid::new_v4().to_string(),
        name: name.trim().into(),
        url: url.clone(),
        url_resolved: url,
        active: true,
        custom: true,
        lastcheckok: 1,
        ..Station::default()
    };
    store.save_custom(&station, Some(backups))?;
    Ok(station)
}

pub fn edit_station(
    store: &mut Store,
    uuid: &str,
    name: Option<String>,
    url: Option<String>,
    backups: &[String],
) -> Result<Station> {
    let mut station = store.station(uuid)?;
    if !station.custom {
        bail!(DomainError::new(ErrorCode::NotCustom));
    }
    if let Some(name) = name {
        station.name = name.trim().into();
    }
    if let Some(url) = url {
        station.url = url.clone();
        station.url_resolved = url;
    }
    validate(&station.name, station.primary_url(), backups)?;
    store.save_custom(&station, (!backups.is_empty()).then_some(backups))?;
    store.station(uuid)
}

fn validate(name: &str, url: &str, backups: &[String]) -> Result<()> {
    if name.trim().is_empty() {
        bail!(DomainError::new(ErrorCode::EmptyStationName));
    }
    validate_url(url)?;
    for backup in backups {
        validate_url(backup)?;
    }
    Ok(())
}

pub fn open_recordings(
    dir: &std::path::Path,
    stop: &std::sync::Arc<std::sync::atomic::AtomicBool>,
) -> Result<()> {
    std::fs::create_dir_all(dir)?;
    let opener = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    crate::process::output(
        std::process::Command::new(opener).arg(dir),
        std::time::Duration::from_secs(5),
        stop,
    )?;
    Ok(())
}

/// Delete files first and the index last. Missing files make retries idempotent;
/// other filesystem errors retain the index for diagnosis and another attempt.
pub fn delete_recording(store: &Store, id: &str, index_only: bool) -> Result<()> {
    let record = store.recording(id)?;
    if matches!(record.status.as_str(), "recording" | "finalizing") || record.recovery_pending {
        bail!(DomainError::new(ErrorCode::RecordingInUse));
    }
    if !index_only {
        for path in [
            std::path::PathBuf::from(&record.path),
            std::path::PathBuf::from(format!("{}.json", record.path)),
        ] {
            match std::fs::remove_file(&path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(
                        anyhow::Error::new(error).context(format!("{} ({id})", path.display()))
                    );
                }
            }
        }
    }
    store.remove_recording_index(id)
}
