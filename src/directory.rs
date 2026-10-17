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
            let result = (|| -> Result<Vec<Station>> {
                let response = self
                    .client
                    .get(format!("{server}{path}"))
                    .query(params)
                    .send()?
                    .error_for_status()?;
                // Limit each response before deserializing, including decompressed data.
                use std::io::Read;
                let mut bytes = Vec::new();
                response
                    .take(16 * 1024 * 1024 + 1)
                    .read_to_end(&mut bytes)?;
                if bytes.len() > 16 * 1024 * 1024 {
                    bail!("目录响应超过 16 MiB");
                }
                Ok(serde_json::from_slice(&bytes)?)
            })();
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

    pub fn sync(&mut self, store: &mut Store, mut progress: impl FnMut(usize)) -> Result<usize> {
        store.begin_sync()?;
        let mut offset = 0usize;
        loop {
            let rows = self.get(
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
            let valid: Vec<_> = rows.into_iter().filter(valid_identity).collect();
            store.stage(&valid)?;
            offset += received;
            progress(offset);
            if received < 1000 {
                break;
            }
            if offset >= 200_000 {
                bail!("目录超过安全上限，请检查服务器分页");
            }
        }
        store.finish_sync()
    }

    pub fn refresh(&mut self, uuid: &str) -> Result<Option<Station>> {
        uuid::Uuid::parse_str(uuid)?;
        Ok(self
            .get(&format!("/json/stations/byuuid/{uuid}"), &[])?
            .into_iter()
            .next())
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
