use crate::{
    config::validate_url,
    error::{DomainError, ErrorCode},
    model::Station,
    store::Store,
};
use anyhow::{Result, bail};

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
