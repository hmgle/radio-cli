mod support;
use radio_cli::{directory::Directory, model::Station, store::Store};
use std::sync::{Arc, atomic::AtomicBool};
use support::{Server, response};

#[test]
fn server_pagination_failure_preserves_previous_snapshot() {
    let station = Station {
        stationuuid: uuid::Uuid::new_v4().to_string(),
        name: "Test".into(),
        active: true,
        ..Station::default()
    };
    let rows = serde_json::to_vec(&vec![station.clone(); 1000]).unwrap();
    let server = Server::new(move |request, stream| {
        assert!(request.to_lowercase().contains("user-agent: radio-cli/"));
        if request.contains("offset=1000") {
            response(stream, 500, "application/json", b"{}");
        } else {
            response(stream, 200, "application/json", &rows);
        }
    });
    let dir = tempfile::tempdir().unwrap();
    let mut config = support::config(dir.path());
    config.settings.api_url = Some(server.url.clone());
    let mut store = Store::open(&dir.path().join("radio.db")).unwrap();
    store.upsert(&station).unwrap();
    let mut client = Directory::new(&config, Arc::new(AtomicBool::new(false))).unwrap();
    assert!(client.sync(&mut store, |_| {}).is_err());
    assert_eq!(store.count().unwrap(), 1);
    assert!(store.meta("last_sync").unwrap().is_none());
}

#[test]
fn sync_fetches_pages_and_deduplicates_uuid() {
    let first = Station {
        stationuuid: uuid::Uuid::new_v4().to_string(),
        name: "First".into(),
        ..Station::default()
    };
    let last = Station {
        stationuuid: uuid::Uuid::new_v4().to_string(),
        name: "Last".into(),
        ..Station::default()
    };
    let page = serde_json::to_vec(&vec![first; 1000]).unwrap();
    let tail = serde_json::to_vec(&vec![last]).unwrap();
    let server = Server::new(move |request, stream| {
        response(
            stream,
            200,
            "application/json",
            if request.contains("offset=1000") {
                &tail
            } else {
                &page
            },
        )
    });
    let dir = tempfile::tempdir().unwrap();
    let mut config = support::config(dir.path());
    config.settings.api_url = Some(server.url.clone());
    let mut store = Store::open(&dir.path().join("radio.db")).unwrap();
    let mut client = Directory::new(&config, Arc::new(AtomicBool::new(false))).unwrap();
    let mut progress = vec![];
    assert_eq!(client.sync(&mut store, |n| progress.push(n)).unwrap(), 2);
    assert_eq!(progress, vec![1000, 1001]);
}

#[test]
fn invalid_upstream_identity_does_not_truncate_pagination() {
    let invalid = Station {
        stationuuid: uuid::Uuid::new_v4().to_string(),
        name: " \t\n".into(),
        ..Station::default()
    };
    let valid = Station {
        stationuuid: uuid::Uuid::new_v4().to_string(),
        name: "Valid tail".into(),
        ..Station::default()
    };
    let page = serde_json::to_vec(&vec![invalid; 1000]).unwrap();
    let tail = serde_json::to_vec(&vec![valid]).unwrap();
    let server = Server::new(move |request, stream| {
        response(
            stream,
            200,
            "application/json",
            if request.contains("offset=1000") {
                &tail
            } else {
                &page
            },
        )
    });
    let dir = tempfile::tempdir().unwrap();
    let mut config = support::config(dir.path());
    config.settings.api_url = Some(server.url.clone());
    let mut store = Store::open(&dir.path().join("radio.db")).unwrap();
    let mut client = Directory::new(&config, Arc::new(AtomicBool::new(false))).unwrap();
    assert_eq!(client.sync(&mut store, |_| {}).unwrap(), 1);
    assert_eq!(store.count().unwrap(), 1);
}
