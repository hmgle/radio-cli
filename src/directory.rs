use crate::{USER_AGENT, config::Config, model::Station, store::Store};
use anyhow::{Context, Result, bail};
use hickory_resolver::Resolver;
use rand::seq::SliceRandom;
use reqwest::blocking::Client;
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

pub struct Directory {
    client: Client,
    servers: Vec<String>,
    explicit: Option<String>,
    stop: Arc<AtomicBool>,
}

impl Directory {
    pub fn new(config: &Config, stop: Arc<AtomicBool>) -> Result<Self> {
        let client = Client::builder()
            .user_agent(USER_AGENT)
            .connect_timeout(Duration::from_secs(config.settings.connect_timeout_secs))
            .timeout(Duration::from_secs(config.settings.request_timeout_secs))
            .build()?;
        Ok(Self {
            client,
            servers: vec![],
            explicit: config.settings.api_url.clone(),
            stop,
        })
    }

    fn discover(&mut self) -> Result<()> {
        if !self.servers.is_empty() {
            return Ok(());
        }
        if let Some(url) = &self.explicit {
            self.servers.push(url.trim_end_matches('/').to_owned());
            return Ok(());
        }
        if let Ok(resolver) = Resolver::from_system_conf()
            && let Ok(records) = resolver.srv_lookup("_api._tcp.radio-browser.info.")
        {
            for record in records.iter() {
                let name = record.target().to_utf8().trim_end_matches('.').to_owned();
                if name.ends_with(".api.radio-browser.info") {
                    self.servers.push(format!("https://{name}"));
                }
            }
        }
        if self.servers.is_empty() {
            #[derive(serde::Deserialize)]
            struct Server {
                name: String,
            }
            let rows: Vec<Server> = self
                .client
                .get("https://all.api.radio-browser.info/json/servers")
                .send()?
                .error_for_status()?
                .json()?;
            for row in rows {
                if row.name.ends_with(".api.radio-browser.info") {
                    self.servers.push(format!("https://{}", row.name));
                }
            }
        }
        self.servers.shuffle(&mut rand::rng());
        if self.servers.is_empty() {
            bail!("未发现可用目录服务器；可设置 api_url");
        }
        Ok(())
    }

    fn get(&mut self, path: &str, params: &[(&str, String)]) -> Result<Vec<Station>> {
        self.discover()?;
        let mut last = None;
        for server in self.servers.iter().take(3) {
            if self.stop.load(Ordering::Relaxed) {
                bail!("操作已取消");
            }
            let result = self.fetch(server, path, params);
            match result {
                Ok(mut rows) => {
                    for row in &mut rows {
                        row.active = true;
                        row.custom = false;
                    }
                    return Ok(rows);
                }
                Err(e) => last = Some(e),
            }
        }
        Err(last.context("目录请求失败")?)
    }

    fn fetch(&self, server: &str, path: &str, params: &[(&str, String)]) -> Result<Vec<Station>> {
        if self.stop.load(Ordering::Relaxed) {
            bail!("操作已取消");
        }
        let response = self
            .client
            .get(format!("{server}{path}"))
            .query(params)
            .send()?
            .error_for_status()?;
        use std::io::Read;
        let mut bytes = Vec::new();
        response
            .take(16 * 1024 * 1024 + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() > 16 * 1024 * 1024 {
            bail!("目录响应超过 16 MiB");
        }
        let mut rows: Vec<Station> = serde_json::from_slice(&bytes)?;
        for row in &mut rows {
            row.active = true;
            row.custom = false;
        }
        Ok(rows)
    }

    pub fn bootstrap(&mut self, store: &Store) -> Result<usize> {
        let rows = self.get(
            "/json/stations/search",
            &[
                ("limit", "1000".into()),
                ("hidebroken", "true".into()),
                ("order", "clickcount".into()),
                ("reverse", "true".into()),
            ],
        )?;
        let mut count = 0;
        if self.stop.load(Ordering::Relaxed) {
            bail!("操作已取消");
        }
        for row in &rows {
            if valid_identity(row) {
                store.upsert(row)?;
                count += 1;
            }
        }
        if count == 0 {
            bail!("目录没有有效电台记录");
        }
        Ok(count)
    }

    pub fn sync(&mut self, store: &mut Store, progress: impl FnMut(usize)) -> Result<usize> {
        self.sync_with_policy(store, progress, false)
    }

    pub fn sync_with_policy(
        &mut self,
        store: &mut Store,
        mut progress: impl FnMut(usize),
        accept_shrink: bool,
    ) -> Result<usize> {
        self.discover()?;
        let previous = store.directory_count()?;
        let mut last = None;
        // A failed page discards the entire staging snapshot before failover.
        for server in self.servers.iter().take(3) {
            store.begin_sync()?;
            progress(0);
            let mut offset = 0usize;
            let mut invalid = 0usize;
            let mut valid = 0usize;
            let result = (|| -> Result<usize> {
                loop {
                    let rows = self.fetch(
                        server,
                        "/json/stations/search",
                        &[
                            ("limit", "1000".into()),
                            ("offset", offset.to_string()),
                            ("hidebroken", "true".into()),
                            ("order", "name".into()),
                        ],
                    )?;
                    if rows.len() > 1000 {
                        bail!("服务器未遵守分页上限");
                    }
                    let received = rows.len();
                    if self.stop.load(Ordering::Relaxed) {
                        bail!("操作已取消");
                    }
                    let rows: Vec<_> = rows.into_iter().filter(valid_identity).collect();
                    valid += rows.len();
                    invalid += received - rows.len();
                    store.stage(&rows)?;
                    offset += received;
                    progress(offset);
                    if received < 1000 {
                        break;
                    }
                    if offset >= 200_000 {
                        bail!("目录超过安全上限，请检查服务器分页");
                    }
                }
                if self.stop.load(Ordering::Relaxed) {
                    bail!("操作已取消");
                }
                let count = store.incoming_count()?;
                let suspicious =
                    previous > 0 && count.saturating_mul(4) < previous.saturating_mul(3);
                let report = serde_json::json!({"server":server,"raw":offset,"valid":valid,"invalid":invalid,"duplicates":valid.saturating_sub(count),"unique":count,"previous":previous,"shrink_requires_acceptance":suspicious,"accepted":accept_shrink});
                store.set_meta("last_sync_attempt", &report.to_string())?;
                if suspicious && !accept_shrink {
                    bail!(
                        "目录由 {previous} 缩减为 {count}，超过 25%；保留旧目录。核实服务器 {server} 后用 sync --accept-shrink 明确接受"
                    );
                }
                store.finish_sync()
            })();
            match result {
                Ok(count) => return Ok(count),
                Err(error) => last = Some(error),
            }
            if self.stop.load(Ordering::Relaxed) {
                bail!("操作已取消");
            }
        }
        Err(last.context("目录同步失败")?)
    }

    pub fn refresh(&mut self, uuid: &str) -> Result<Option<Station>> {
        uuid::Uuid::parse_str(uuid)?;
        Ok(self
            .get(&format!("/json/stations/byuuid/{uuid}"), &[])?
            .into_iter()
            .find(|row| row.stationuuid == uuid && valid_identity(row)))
    }

    pub fn click(&mut self, uuid: &str) -> Result<()> {
        uuid::Uuid::parse_str(uuid)?;
        self.discover()?;
        self.client
            .post(format!("{}/json/url/{uuid}", self.servers[0]))
            .timeout(Duration::from_secs(3))
            .send()?
            .error_for_status()?;
        Ok(())
    }
}

fn valid_identity(station: &Station) -> bool {
    !crate::clean_text(&station.name).trim().is_empty()
        && uuid::Uuid::parse_str(&station.stationuuid).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{Read, Write},
        net::TcpListener,
        thread,
    };

    fn server(responses: Vec<(u16, Vec<u8>)>) -> (String, thread::JoinHandle<Vec<String>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let handle = thread::spawn(move || {
            let mut requests = Vec::new();
            for (status, body) in responses {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let mut bytes = [0; 8192];
                let n = stream.read(&mut bytes).unwrap();
                requests.push(String::from_utf8_lossy(&bytes[..n]).into());
                write!(stream, "HTTP/1.1 {status} OK\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n", body.len()).unwrap();
                stream.write_all(&body).unwrap();
            }
            requests
        });
        (url, handle)
    }

    #[test]
    fn failover_restarts_the_whole_snapshot() {
        let first = Station {
            stationuuid: uuid::Uuid::new_v4().to_string(),
            name: "Only A".into(),
            ..Station::default()
        };
        let second = Station {
            stationuuid: uuid::Uuid::new_v4().to_string(),
            name: "Only B".into(),
            ..Station::default()
        };
        let (a, a_handle) = server(vec![
            (200, serde_json::to_vec(&vec![first.clone(); 1000]).unwrap()),
            (500, b"{}".to_vec()),
        ]);
        let (b, b_handle) = server(vec![(
            200,
            serde_json::to_vec(&vec![second.clone()]).unwrap(),
        )]);
        let dir = tempfile::tempdir().unwrap();
        let config = Config::load(None, Some(dir.path()), Some(&a)).unwrap();
        let mut store = Store::open(&dir.path().join("radio.db")).unwrap();
        let mut directory = Directory::new(&config, Arc::new(AtomicBool::new(false))).unwrap();
        directory.servers = vec![a, b];
        assert_eq!(directory.sync(&mut store, |_| {}).unwrap(), 1);
        assert!(store.station(&first.stationuuid).is_err());
        assert!(store.station(&second.stationuuid).unwrap().active);
        assert!(a_handle.join().unwrap()[1].contains("offset=1000"));
        assert!(b_handle.join().unwrap()[0].contains("offset=0"));
    }
}
