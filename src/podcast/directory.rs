//! Public, regional podcast charts with lazy RSS resolution on selection.
use super::{error, feed};
use crate::{config::Config, store::Store};
use anyhow::Result;
use rand::seq::SliceRandom;
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

// Apple Podcasts' top-level category IDs. Keep slugs stable for CLI scripts.
pub const CATEGORIES: &[(&str, &str, &str, &str)] = &[
    ("1301", "arts", "Arts", "艺术"),
    ("1321", "business", "Business", "商务"),
    ("1303", "comedy", "Comedy", "喜剧"),
    ("1304", "education", "Education", "教育"),
    ("1483", "fiction", "Fiction", "小说"),
    ("1511", "government", "Government", "政府"),
    ("1512", "health-fitness", "Health & Fitness", "健康与健身"),
    ("1487", "history", "History", "历史"),
    ("1305", "kids-family", "Kids & Family", "儿童与家庭"),
    ("1502", "leisure", "Leisure", "休闲"),
    ("1310", "music", "Music", "音乐"),
    ("1489", "news", "News", "新闻"),
    (
        "1314",
        "religion-spirituality",
        "Religion & Spirituality",
        "宗教与心灵",
    ),
    ("1533", "science", "Science", "科学"),
    ("1324", "society-culture", "Society & Culture", "社会与文化"),
    ("1545", "sports", "Sports", "体育"),
    ("1318", "technology", "Technology", "科技"),
    ("1488", "true-crime", "True Crime", "犯罪纪实"),
    ("1309", "tv-film", "TV & Film", "电视与电影"),
];

pub fn category_id(value: &str) -> std::result::Result<String, String> {
    CATEGORIES
        .iter()
        .find(|(id, slug, _, _)| *id == value || *slug == value)
        .map(|(id, _, _, _)| (*id).into())
        .ok_or_else(|| crate::t!("podcast_invalid_category").into())
}

pub fn country(config: &Config, value: &str) -> Result<String> {
    let value = if value.is_empty() {
        config.settings.podcasts.country.as_deref().unwrap_or(
            if crate::messages::locale() == crate::messages::Locale::ZhCn {
                "CN"
            } else {
                "US"
            },
        )
    } else {
        value
    };
    if value.len() != 2 || !value.bytes().all(|c| c.is_ascii_alphabetic()) {
        return Err(error(crate::t!("podcast_invalid_country")));
    }
    Ok(value.to_ascii_uppercase())
}

pub fn categories(country: &str) -> Value {
    let zh = crate::messages::locale() == crate::messages::Locale::ZhCn;
    let rows: Vec<_> = CATEGORIES
        .iter()
        .map(|(id, slug, en, cn)| {
            json!({
                "id": id, "slug": slug, "title": if zh {cn} else {en},
                "kind": "category", "meta": slug,
                "command": format!("podcast charts --country {country} --category {slug}"),
            })
        })
        .collect();
    json!({"items": rows, "total": rows.len(), "feed_title": format!("{} · {country}", crate::t!("podcast_categories"))})
}

pub fn home(country: &str) -> Value {
    let rows = vec![
        json!({"title": crate::t!("podcast_charts"), "command": format!("podcast charts --country {country}")}),
        json!({"title": crate::t!("podcast_categories"), "command": format!("podcast categories --country {country}")}),
        json!({"title": crate::t!("podcast_surprise"), "description": crate::t!("podcast_random_scope"), "command": format!("podcast charts --country {country} --random")}),
        json!({"title": crate::t!("podcast_search_prompt"), "command": format!("podcast search --country {country} "), "input": true}),
        json!({"title": crate::t!("podcast_region"), "command": "podcast browse --country ", "input": true}),
        json!({"title": crate::t!("podcast_subscriptions"), "command": "podcast list"}),
    ];
    let rows: Vec<_> = rows
        .into_iter()
        .map(|mut v| {
            v["kind"] = json!("directory");
            v
        })
        .collect();
    json!({"items": rows, "total": rows.len(), "feed_title": format!("{} · {country}", crate::t!("podcast_browse"))})
}

fn parse_chart(value: &Value, country: &str) -> Result<Vec<Value>> {
    let feed = value
        .get("feed")
        .and_then(Value::as_object)
        .ok_or_else(|| error(crate::t!("podcast_invalid_feed")))?;
    // Apple omits entry when a storefront/category has no chart.
    let empty = vec![];
    let entries = match feed.get("entry") {
        None => &empty,
        Some(Value::Array(rows)) => rows,
        _ => return Err(error(crate::t!("podcast_invalid_feed"))),
    };
    let mut seen = HashSet::new();
    let rows = entries.iter().take(100).enumerate().filter_map(|(rank, entry)| {
        let id = entry["id"]["attributes"]["im:id"].as_str()?;
        if id.is_empty() || !id.bytes().all(|c| c.is_ascii_digit()) || !seen.insert(id.to_owned()) {
            return None;
        }
        let title = entry["im:name"]["label"].as_str().filter(|s| !s.is_empty())?;
        Some(json!({
            "collectionId": id, "collectionName": title,
            "artistName": entry["im:artist"]["label"].as_str().unwrap_or(""),
            "collectionViewUrl": format!("https://podcasts.apple.com/{}/podcast/id{id}", country.to_ascii_lowercase()),
            "description": entry["summary"]["label"].as_str().unwrap_or(""),
            "primaryGenreName": entry["category"]["attributes"]["label"].as_str().unwrap_or(""),
            "chartRank": rank + 1,
        }))
    }).collect::<Vec<_>>();
    if !entries.is_empty() && rows.is_empty() {
        return Err(error(crate::t!("podcast_invalid_feed")));
    }
    Ok(rows)
}

#[derive(Default)]
struct Cache {
    pages: HashMap<String, (Instant, Vec<Value>)>,
    retry_at: Option<Instant>,
}
static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();

fn fetch_chart(
    config: &Config,
    url: &str,
    country: &str,
    stop: &Arc<AtomicBool>,
) -> Result<Vec<Value>> {
    if stop.load(Ordering::Relaxed) {
        return Err(crate::error::DomainError::new(crate::error::ErrorCode::Cancelled).into());
    }
    let cache = CACHE.get_or_init(Mutex::default);
    {
        let cache = cache.lock().unwrap();
        if let Some((at, rows)) = cache.pages.get(url)
            && at.elapsed() < Duration::from_secs(600)
        {
            return Ok(rows.clone());
        }
        if cache.retry_at.is_some_and(|at| at > Instant::now()) {
            return Err(error(crate::t!("podcast_rate_limited")));
        }
    }
    let response = feed::client(config)?
        .get(url)
        .send()
        .map_err(feed::network_error)?;
    if response.status() == 429 {
        let seconds = response
            .headers()
            .get("retry-after")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(60)
            .clamp(1, 3600);
        cache.lock().unwrap().retry_at = Some(Instant::now() + Duration::from_secs(seconds));
        return Err(error(crate::t!("podcast_rate_limited")));
    }
    let (_, data) = feed::body(response, stop)?;
    let value =
        serde_json::from_slice(&data).map_err(|_| error(crate::t!("podcast_invalid_feed")))?;
    let rows = parse_chart(&value, country)?;
    let mut cache = cache.lock().unwrap();
    if cache.pages.len() >= 32 {
        cache.pages.clear();
    }
    cache
        .pages
        .insert(url.into(), (Instant::now(), rows.clone()));
    Ok(rows)
}

pub fn annotate_subscriptions(store: &Store, rows: &mut [Value]) -> Result<()> {
    for row in rows {
        let url = row["feedUrl"]
            .as_str()
            .or_else(|| row["collectionViewUrl"].as_str());
        if let Some(feed) = url
            .map(|url| store.podcast_by_url(url))
            .transpose()?
            .flatten()
            && feed.subscribed
        {
            row["local_feed_id"] = json!(feed.id);
        }
    }
    Ok(())
}

pub fn charts(
    config: &Config,
    store: &Store,
    country: &str,
    category: Option<&str>,
    limit: u16,
    random: bool,
    stop: &Arc<AtomicBool>,
) -> Result<Value> {
    let genre = category
        .map(category_id)
        .transpose()
        .map_err(|e| error(&e))?;
    let url = format!(
        "https://itunes.apple.com/{}/rss/toppodcasts/limit=100{}/json",
        country.to_ascii_lowercase(),
        genre
            .as_ref()
            .map(|id| format!("/genre={id}"))
            .unwrap_or_default()
    );
    let mut rows = fetch_chart(config, &url, country, stop)?;
    if random {
        rows.shuffle(&mut rand::rng());
    }
    rows.truncate(usize::from(limit));
    annotate_subscriptions(store, &mut rows)?;
    if random {
        for row in &mut rows {
            row["description"] = json!(format!(
                "{}\n\n{}",
                crate::t!("podcast_random_scope"),
                row["description"].as_str().unwrap_or("")
            ));
        }
    }
    let label = if random {
        crate::t!("podcast_surprise")
    } else {
        crate::t!("podcast_charts")
    };
    let category_label = genre
        .as_ref()
        .and_then(|id| CATEGORIES.iter().find(|(key, _, _, _)| key == id))
        .map(|(_, _, en, cn)| {
            if crate::messages::locale() == crate::messages::Locale::ZhCn {
                *cn
            } else {
                *en
            }
        });
    Ok(
        json!({"items": rows, "total": rows.len(), "country": country,
        "category": genre, "random": random, "source": "Apple Podcasts",
        "feed_title": format!("{label} · {country}{} · Apple Podcasts", category_label.map(|name| format!(" · {name}")).unwrap_or_default())}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn random_candidates_keep_rank_identity_and_leave_the_chart_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let config = crate::test_support::config(dir.path());
        let store = Store::open(&config.data_dir.join("radio.db")).unwrap();
        let url = "https://itunes.apple.com/zz/rss/toppodcasts/limit=100/genre=1318/json";
        let rows = (1..=100).map(|n| json!({"collectionId":n.to_string(),"collectionName":format!("Show {n}"),"chartRank":n})).collect();
        CACHE
            .get_or_init(Mutex::default)
            .lock()
            .unwrap()
            .pages
            .insert(url.into(), (Instant::now(), rows));
        let stop = Arc::new(AtomicBool::new(false));
        let random = charts(&config, &store, "ZZ", Some("technology"), 10, true, &stop).unwrap();
        let rows = random["items"].as_array().unwrap();
        assert_eq!(rows.len(), 10);
        let ids = rows
            .iter()
            .map(|row| row["collectionId"].as_str().unwrap())
            .collect::<HashSet<_>>();
        assert_eq!(ids.len(), 10);
        for row in rows {
            assert_eq!(
                row["collectionId"].as_str().unwrap(),
                row["chartRank"].to_string()
            );
            assert!((1..=100).contains(&row["chartRank"].as_u64().unwrap()));
        }
        let chart = charts(&config, &store, "ZZ", Some("1318"), 10, false, &stop).unwrap();
        assert_eq!(chart["items"][0]["chartRank"], 1);
        assert_eq!(chart["items"][9]["chartRank"], 10);
        CACHE.get().unwrap().lock().unwrap().pages.remove(url);
    }
    #[test]
    fn chart_cache_errors_cancellation_and_cooldown_are_bounded() {
        use std::{
            io::{Read, Write},
            net::TcpListener,
            thread,
        };
        let dir = tempfile::tempdir().unwrap();
        let config = crate::test_support::config(dir.path());
        let stop = Arc::new(AtomicBool::new(false));
        let serve = |status: &str, body: &str| {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let url = format!("http://{}", listener.local_addr().unwrap());
            let reply = format!(
                "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\nRetry-After: 60\r\n\r\n{body}",
                body.len()
            );
            let task = thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let mut buf = [0; 4096];
                let n = stream.read(&mut buf).unwrap();
                assert!(n > 0);
                stream.write_all(reply.as_bytes()).unwrap();
            });
            (url, task)
        };
        let (url, task) = serve(
            "200 OK",
            r#"{"feed":{"entry":[{"id":{"attributes":{"im:id":"123"}},"im:name":{"label":"Example"}}]}}"#,
        );
        let rows = fetch_chart(&config, &url, "US", &stop).unwrap();
        task.join().unwrap();
        assert_eq!(fetch_chart(&config, &url, "US", &stop).unwrap(), rows);
        stop.store(true, Ordering::Relaxed);
        assert!(fetch_chart(&config, &url, "US", &stop).is_err());
        stop.store(false, Ordering::Relaxed);
        let (bad, task) = serve("200 OK", "not json");
        assert!(fetch_chart(&config, &bad, "US", &stop).is_err());
        task.join().unwrap();
        assert!(
            !CACHE
                .get()
                .unwrap()
                .lock()
                .unwrap()
                .pages
                .contains_key(&bad)
        );
        let (limited, task) = serve("429 Too Many Requests", "");
        assert!(fetch_chart(&config, &limited, "US", &stop).is_err());
        task.join().unwrap();
        let err = fetch_chart(&config, "http://127.0.0.1:1/uncontacted", "US", &stop).unwrap_err();
        assert!(err.to_string().contains(crate::t!("podcast_rate_limited")));
        CACHE.get().unwrap().lock().unwrap().retry_at = None;
    }
    #[test]
    fn charts_preserve_rank_and_resolve_only_when_selected() {
        let value = json!({"feed":{"entry":[
            {"id":{"attributes":{"im:id":"123"}},"im:name":{"label":"科技"},"im:artist":{"label":"作者"}},
            {"id":{"attributes":{"im:id":"123"}},"im:name":{"label":"Duplicate"}},
            {"id":{"attributes":{"im:id":"456"}},"im:name":{"label":"Second"}},
            {"id":{"attributes":{"im:id":"../bad"}},"im:name":{"label":"Invalid"}}
        ]}});
        let rows = parse_chart(&value, "CN").unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1]["chartRank"], 3);
        assert_eq!(
            rows[0]["collectionViewUrl"],
            "https://podcasts.apple.com/cn/podcast/id123"
        );
        assert!(rows[0].get("feedUrl").is_none());
        assert!(parse_chart(&json!({"feed":{}}), "US").unwrap().is_empty());
        assert!(parse_chart(&json!({"error":"not a chart"}), "US").is_err());
        assert!(parse_chart(&json!({"feed":{"entry":{}}}), "US").is_err());
    }
    #[test]
    fn directory_options_are_validated_before_networking() {
        let dir = tempfile::tempdir().unwrap();
        let config = crate::test_support::config(dir.path());
        assert_eq!(country(&config, "cn").unwrap(), "CN");
        assert!(country(&config, "../cn").is_err());
        assert_eq!(category_id("technology").unwrap(), "1318");
        assert_eq!(category_id("1318").unwrap(), "1318");
        assert!(category_id("not-a-category").is_err());
        let cancelled = Arc::new(AtomicBool::new(true));
        assert!(fetch_chart(&config, "https://example.invalid", "CN", &cancelled).is_err());
    }
}
