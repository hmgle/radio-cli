use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct Station {
    pub stationuuid: String,
    pub name: String,
    pub url: String,
    pub url_resolved: String,
    pub homepage: String,
    pub country: String,
    pub countrycode: String,
    pub state: String,
    pub language: String,
    pub languagecodes: String,
    pub tags: String,
    pub codec: String,
    pub bitrate: u32,
    pub hls: u8,
    pub lastcheckok: u8,
    pub votes: u64,
    pub clickcount: u64,
    pub custom: bool,
    pub active: bool,
    pub favorite: bool,
}

impl Station {
    pub fn primary_url(&self) -> &str {
        if self.url_resolved.is_empty() {
            &self.url
        } else {
            &self.url_resolved
        }
    }

    pub fn stream_candidates(&self, backups: &[String]) -> Vec<String> {
        let mut result = Vec::new();
        for url in std::iter::once(self.primary_url())
            .chain(std::iter::once(self.url.as_str()))
            .chain(backups.iter().map(String::as_str))
        {
            if crate::config::validate_url(url).is_ok() && !result.iter().any(|s| s == url) {
                result.push(url.to_owned());
            }
        }
        result
    }

    pub fn search_text(&self) -> String {
        format!(
            "{} {} {} {} {} {} {}",
            self.name,
            self.country,
            self.countrycode,
            self.state,
            self.language,
            self.languagecodes,
            self.tags
        )
        .to_lowercase()
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Filter {
    pub query: String,
    pub country: Option<String>,
    pub language: Option<String>,
    pub tag: Option<String>,
    pub min_bitrate: u32,
    pub favorites: bool,
    pub history: bool,
    pub custom: bool,
    pub limit: usize,
    pub offset: usize,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Recording {
    pub id: String,
    pub stationuuid: String,
    pub station_name: String,
    pub source_url_resolved: String,
    pub started_at: String,
    pub stopped_at: Option<String>,
    pub media_duration_ms: Option<u64>,
    pub codec: String,
    pub container: String,
    pub path: String,
    #[serde(default)]
    pub final_path: Option<String>,
    pub size_bytes: u64,
    pub status: String,
    pub error: Option<String>,
    #[serde(default)]
    pub recovery_pending: bool,
}
