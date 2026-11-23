use super::*;
use crate::config::Config;
use anyhow::Result;
use quick_xml::{NsReader, events::Event, name::ResolveResult};
use reqwest::{
    Url,
    blocking::{Client, Response},
};
use scraper::{Html, Selector};
use std::{
    error::Error as _,
    io::{self, Read},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
const LIMIT: u64 = 10 * 1024 * 1024;
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Preview {
    pub feed: Feed,
    pub episodes: Vec<Episode>,
}
pub(crate) fn url(value: &str, base: Option<&Url>) -> Result<Url> {
    let mut u = match base {
        Some(b) => b.join(value),
        None => Url::parse(value),
    }
    .map_err(|_| error(crate::t!("podcast_invalid_url")))?;
    if !matches!(u.scheme(), "http" | "https")
        || u.host_str().is_none()
        || !u.username().is_empty()
        || u.password().is_some()
    {
        return Err(error(crate::t!("podcast_invalid_url")));
    }
    u.set_fragment(None);
    Ok(u)
}
pub(crate) fn client(config: &Config) -> Result<Client> {
    Client::builder()
        .user_agent(crate::USER_AGENT)
        .connect_timeout(Duration::from_secs(config.settings.connect_timeout_secs))
        .timeout(Duration::from_secs(config.settings.request_timeout_secs))
        .redirect(reqwest::redirect::Policy::custom(|a| {
            if a.previous().len() >= 5
                || !matches!(a.url().scheme(), "http" | "https")
                || !a.url().username().is_empty()
                || a.url().password().is_some()
            {
                a.stop()
            } else {
                a.follow()
            }
        }))
        .build()
        .map_err(network_error)
}
pub(crate) fn body(mut r: Response, stop: &AtomicBool) -> Result<(Url, Vec<u8>)> {
    if !r.status().is_success() {
        return Err(error(&format!(
            "{} {}",
            crate::t!("podcast_http"),
            r.status().as_u16()
        )));
    }
    let base = r.url().clone();
    let mut data = Vec::new();
    let mut buf = [0; 16384];
    loop {
        if stop.load(Ordering::Relaxed) {
            return Err(DomainError::new(ErrorCode::Cancelled).into());
        }
        let n = r
            .read(&mut buf)
            .map_err(|e| error(&format!("{}: {}", crate::t!("podcast_network"), e.kind())))?;
        if n == 0 {
            break;
        }
        if data.len() as u64 + n as u64 > LIMIT {
            return Err(error(crate::t!("podcast_limit")));
        }
        data.extend_from_slice(&buf[..n]);
    }
    Ok((base, data))
}
pub fn fetch(
    config: &Config,
    value: &str,
    old: Option<&Feed>,
    stop: &Arc<AtomicBool>,
) -> Result<Option<Preview>> {
    let client = Client::builder()
        .user_agent(crate::USER_AGENT)
        .connect_timeout(Duration::from_secs(config.settings.connect_timeout_secs))
        .timeout(Duration::from_secs(config.settings.request_timeout_secs))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(network_error)?;
    let mut current = url(value, None)?;
    let mut canonical = current.clone();
    let mut visited = Vec::new();
    for _ in 0..=5 {
        if stop.load(Ordering::Relaxed) {
            return Err(DomainError::new(ErrorCode::Cancelled).into());
        }
        if visited.contains(&current) {
            return Err(error(crate::t!("podcast_invalid_url")));
        }
        visited.push(current.clone());
        let mut req = client.get(current.clone());
        // Validators are only reusable for the URL that supplied them.
        if let Some(f) = old.filter(|f| f.url == current.as_str()) {
            if let Some(v) = &f.etag {
                req = req.header("If-None-Match", v);
            }
            if let Some(v) = &f.modified {
                req = req.header("If-Modified-Since", v);
            }
        }
        let r = req.send().map_err(network_error)?;
        if r.status() == 304 && old.is_some() {
            return Ok(None);
        }
        if matches!(r.status().as_u16(), 301 | 302 | 303 | 307 | 308) {
            let next = r
                .headers()
                .get("location")
                .and_then(|v| v.to_str().ok())
                .ok_or_else(|| error(crate::t!("podcast_invalid_url")))?;
            let next = url(next, Some(&current))?;
            // Do not persist a permanent redirect reached through a temporary URL.
            if matches!(r.status().as_u16(), 301 | 308) && canonical == current {
                canonical = next.clone();
            }
            current = next;
            continue;
        }
        let etag = r
            .headers()
            .get("etag")
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        let modified = r
            .headers()
            .get("last-modified")
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        let (base, data) = body(r, stop)?;
        let mut p = parse(&data, &base)?;
        if let Some(next) = p.feed.new_feed_url.as_deref() {
            let next = url(next, Some(&base))?;
            if next != current {
                canonical = next.clone();
                current = next;
                continue;
            }
        }
        p.feed.url = canonical.to_string();
        if canonical == current {
            p.feed.etag = etag;
            p.feed.modified = modified;
        }
        p.feed.refreshed_at = Some(chrono::Utc::now().to_rfc3339());
        return Ok(Some(p));
    }
    Err(error(crate::t!("podcast_limit")))
}

pub(crate) fn network_error(e: reqwest::Error) -> anyhow::Error {
    // Sources can include signed URLs or proxy credentials. Only expose typed
    // causes, using the standard I/O category instead of the source message.
    let reason = if e.is_timeout() {
        Some(crate::t!("podcast_network_timeout").to_owned())
    } else {
        std::iter::successors(e.source(), |&source| source.source())
            .filter_map(|source| source.downcast_ref::<io::Error>())
            .map(io::Error::kind)
            .find(|kind| *kind != io::ErrorKind::Other)
            .map(|kind| kind.to_string())
            .or_else(|| {
                e.is_connect()
                    .then(|| crate::t!("podcast_network_connect").to_owned())
            })
    };
    let mut message = format!("{}: {}", crate::t!("podcast_network"), e.without_url());
    if let Some(reason) = reason {
        message.push_str(": ");
        message.push_str(&reason);
    }
    error(&message)
}

fn plain(value: &str) -> String {
    Html::parse_fragment(value)
        .root_element()
        .text()
        .collect::<Vec<_>>()
        .join(" ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}
fn notes(value: &str) -> String {
    fn walk(element: scraper::ElementRef<'_>, output: &mut String) {
        let name = element.value().name();
        if matches!(name, "script" | "style") {
            return;
        }
        let block = matches!(
            name,
            "p" | "div" | "li" | "ul" | "ol" | "h1" | "h2" | "h3" | "br"
        );
        if block && !output.ends_with('\n') {
            output.push('\n');
        }
        if name == "li" {
            output.push_str("• ");
        }
        for child in element.children() {
            if let Some(text) = child.value().as_text() {
                output.push_str(&text.split_whitespace().collect::<Vec<_>>().join(" "));
                output.push(' ');
            } else if let Some(child) = scraper::ElementRef::wrap(child) {
                walk(child, output);
            }
        }
        if name == "a"
            && let Some(href) = element
                .value()
                .attr("href")
                .filter(|u| u.starts_with("https://") || u.starts_with("http://"))
        {
            output.push_str(&format!(" ({href}) "));
        }
        if block && !output.ends_with('\n') {
            output.push('\n');
        }
    }
    let html = Html::parse_fragment(value);
    let mut output = String::new();
    walk(html.root_element(), &mut output);
    output
        .lines()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n")
}
fn duration(value: &str) -> Option<u64> {
    super::parse_time(value.trim()).ok()
}
fn published(value: &str) -> Option<String> {
    let strict = |s: &str| {
        chrono::DateTime::parse_from_rfc2822(s)
            .or_else(|_| chrono::DateTime::parse_from_rfc3339(s))
            .ok()
    };
    if let Some(d) = strict(value) {
        return Some(d.to_utc().to_rfc3339());
    }
    let value = value.split_once(',').map_or(value, |(_, rest)| rest.trim());
    let mut parts: Vec<String> = value.split_whitespace().map(str::to_owned).collect();
    if let Some(zone) = parts.last_mut() {
        *zone = match zone.as_str() {
            "UTC" => "+0000".into(),
            "CEST" => "+0200".into(),
            z if (z.starts_with('+') || z.starts_with('-')) && z.len() == 6 => z.replace(':', ""),
            z => z.into(),
        };
    }
    strict(&parts.join(" ")).map(|d| d.to_utc().to_rfc3339())
}

struct Node {
    name: String,
    text: String,
    base: Url,
    html: bool,
    updated: Option<String>,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Scope {
    Feed,
    Episode,
    Other,
}
fn scope(stack: &[Node]) -> Scope {
    match stack {
        [feed] if feed.name == "feed" => Scope::Feed,
        [rss, channel] if rss.name == "rss" && channel.name == "channel" => Scope::Feed,
        [feed, entry] if feed.name == "feed" && entry.name == "entry" => Scope::Episode,
        [rss, channel, item]
            if rss.name == "rss" && channel.name == "channel" && item.name == "item" =>
        {
            Scope::Episode
        }
        _ => Scope::Other,
    }
}
pub(crate) fn parse(data: &[u8], base: &Url) -> Result<Preview> {
    if data.len() as u64 > LIMIT {
        return Err(error(crate::t!("podcast_limit")));
    }
    if data.starts_with(&[0xff, 0xfe]) || data.starts_with(&[0xfe, 0xff]) {
        return Err(error(crate::t!("podcast_invalid_feed")));
    }
    let invalid = || error(crate::t!("podcast_invalid_feed"));
    let mut reader = NsReader::from_reader(data);
    reader.config_mut().check_end_names = true;
    let mut stack: Vec<Node> = vec![];
    let mut f = Feed {
        url: base.to_string(),
        ..Default::default()
    };
    let mut episodes = vec![];
    let mut current: Option<Episode> = None;
    let mut count = 0;
    let mut root = String::new();
    loop {
        match reader.read_event().map_err(|_| invalid())? {
            Event::Start(e) | Event::Empty(e) => {
                let empty =
                    data.get(reader.buffer_position().saturating_sub(2) as usize) == Some(&b'/');
                let name = e.local_name().as_ref().to_owned();
                if stack.is_empty() {
                    if !root.is_empty() || !matches!(name.as_str(), "rss" | "feed") {
                        return Err(invalid());
                    }
                    root = name.clone();
                }
                if stack.len() >= 64 {
                    return Err(error(crate::t!("podcast_limit")));
                }
                let parent_base = stack.last().map(|n| &n.base).unwrap_or(base);
                let mut attrs = std::collections::HashMap::new();
                for a in e.attributes() {
                    let a = a.map_err(|_| invalid())?;
                    attrs.insert(
                        a.key.as_ref().to_owned(),
                        a.normalized_value(quick_xml::XmlVersion::Implicit1_0)
                            .map_err(|_| invalid())?
                            .into_owned(),
                    );
                }
                let node_base = if let Some(b) = attrs.get("xml:base") {
                    url(b, Some(parent_base))?
                } else {
                    parent_base.clone()
                };
                let attr_url = |key: &str| {
                    attrs
                        .get(key)
                        .and_then(|v| url(v, Some(&node_base)).ok())
                        .map(|u| u.to_string())
                };
                let parent_scope = scope(&stack);
                if parent_scope == Scope::Feed
                    && ((root == "feed" && name == "entry") || (root == "rss" && name == "item"))
                {
                    count += 1;
                    if count > 10000 || current.is_some() {
                        return Err(error(crate::t!("podcast_limit")));
                    }
                    current = Some(Episode::default());
                }
                if let Some(ep) = current.as_mut() {
                    let ns = reader.resolver().resolve_element(e.name()).0;
                    if name == "chapters"
                        && parent_scope == Scope::Episode
                        && matches!(ns, ResolveResult::Bound(ns) if ns.as_ref() == "https://podcastindex.org/namespace/1.0")
                    {
                        ep.chapters_url = attr_url("url");
                    } else if name == "chapter"
                        && matches!(ns, ResolveResult::Bound(ns) if ns.as_ref() == "http://podlove.org/simple-chapters")
                        && let Some(position_ms) =
                            attrs.get("start").and_then(|v| super::parse_time(v).ok())
                    {
                        ep.chapters.push(Chapter {
                            title: crate::clean_text(
                                attrs.get("title").map(String::as_str).unwrap_or(""),
                            ),
                            position_ms,
                        });
                    }
                }
                if parent_scope == Scope::Episode
                    && name == "transcript"
                    && matches!(reader.resolver().resolve_element(e.name()).0, ResolveResult::Bound(ns) if ns.as_ref() == "https://podcastindex.org/namespace/1.0")
                    && let Some(ep) = current.as_mut()
                    && let Some(url) = attr_url("url")
                {
                    ep.transcripts.push(Transcript {
                        url,
                        mime: attrs.get("type").cloned().unwrap_or_default(),
                        language: attrs.get("language").cloned(),
                    });
                }
                if parent_scope == Scope::Episode
                    && (name == "enclosure"
                        || (name == "link" && attrs.get("rel").is_some_and(|r| r == "enclosure")))
                {
                    if let Some(ep) = current.as_mut() {
                        let mime = attrs.get("type").cloned().unwrap_or_default();
                        if ep.url.is_empty()
                            && (mime.is_empty()
                                || mime.starts_with("audio/")
                                || mime == "application/octet-stream"
                                || mime == "application/ogg")
                            && let Some(u) = attr_url(if name == "link" { "href" } else { "url" })
                        {
                            ep.url = u;
                            ep.mime = mime;
                            ep.bytes = attrs.get("length").and_then(|v| v.parse().ok());
                        }
                    }
                } else if parent_scope == Scope::Feed
                    && name == "image"
                    && let Some(u) = attr_url("href")
                {
                    f.image = u;
                } else if matches!(parent_scope, Scope::Feed | Scope::Episode)
                    && name == "link"
                    && attrs.get("rel").is_none_or(|v| v == "alternate")
                    && let Some(u) = attr_url("href")
                {
                    if let Some(e) = current.as_mut() {
                        e.website = u;
                    } else {
                        f.website = u;
                    }
                }
                stack.push(Node {
                    name,
                    text: String::new(),
                    base: node_base,
                    // XHTML tags are consumed by the XML reader. Only HTML
                    // needs another parsing pass after XML entity decoding.
                    html: root != "feed"
                        || attrs
                            .get("type")
                            .is_some_and(|t| matches!(t.as_str(), "html" | "text/html")),
                    updated: None,
                });
                if !empty {
                    continue;
                }
                finish(&mut stack, &mut f, &mut current, &mut episodes);
            }
            Event::End(_) => finish(&mut stack, &mut f, &mut current, &mut episodes),
            Event::Text(t) => {
                if let Some(n) = stack.last_mut() {
                    n.text.push_str(t.as_ref());
                }
            }
            Event::CData(t) => {
                if let Some(n) = stack.last_mut() {
                    n.text.push_str(t.as_ref());
                }
            }
            Event::GeneralRef(t) => {
                if let Some(n) = stack.last_mut() {
                    let t = t.as_ref();
                    let v = quick_xml::escape::unescape(&format!("&{t};"))
                        .map_err(|_| invalid())?
                        .into_owned();
                    n.text.push_str(&v);
                }
            }
            Event::DocType(_) => return Err(invalid()),
            Event::Eof => break,
            _ => {}
        }
    }
    if !stack.is_empty() || root.is_empty() || f.title.is_empty() || episodes.is_empty() {
        return Err(invalid());
    }
    Ok(Preview { feed: f, episodes })
}
fn finish(
    stack: &mut Vec<Node>,
    f: &mut Feed,
    current: &mut Option<Episode>,
    episodes: &mut Vec<Episode>,
) {
    let Some(n) = stack.pop() else { return };
    let text = n.text.trim();
    let display_text = || {
        if n.html {
            plain(text)
        } else {
            text.split_whitespace().collect::<Vec<_>>().join(" ")
        }
    };
    let parent = stack.last().map(|n| n.name.as_str()).unwrap_or("");
    let parent_scope = scope(stack);
    let resolved = || {
        url(text, Some(&n.base))
            .map(|u| u.to_string())
            .unwrap_or_default()
    };
    if let Some(e) = current.as_mut() {
        if parent_scope == Scope::Episode {
            match n.name.as_str() {
                "title" => e.title = display_text(),
                "guid" | "id" if !text.is_empty() => e.guid = Some(text.into()),
                "description" | "summary" | "content" => {
                    e.description = if n.html { notes(text) } else { text.to_owned() }
                }
                "pubDate" | "published" => {
                    e.published = published(text);
                }
                "updated" => {
                    stack.last_mut().unwrap().updated = chrono::DateTime::parse_from_rfc3339(text)
                        .ok()
                        .map(|d| d.to_utc().to_rfc3339());
                }
                "link" if !text.is_empty() => e.website = resolved(),
                "duration" => e.duration_ms = duration(text),
                _ => {}
            }
        }
        if parent_scope == Scope::Feed && matches!(n.name.as_str(), "item" | "entry") {
            let mut e = current.take().unwrap();
            e.published = e.published.or(n.updated);
            if !e.url.is_empty() {
                e.source_order = Some(episodes.len() as u64);
                episodes.push(e);
            }
        }
    } else {
        let nested_feed_field = stack
            .split_last()
            .is_some_and(|(_, ancestors)| scope(ancestors) == Scope::Feed);
        match n.name.as_str() {
            "new-feed-url" if parent_scope == Scope::Feed => f.new_feed_url = Some(resolved()),
            "title" if parent_scope == Scope::Feed => f.title = display_text(),
            "author" | "managingEditor" if parent_scope == Scope::Feed => f.author = display_text(),
            "name" if parent == "author" && nested_feed_field => f.author = display_text(),
            "description" | "subtitle" if parent_scope == Scope::Feed => {
                f.description = if n.html { notes(text) } else { text.to_owned() }
            }
            "link" if parent_scope == Scope::Feed && !text.is_empty() => f.website = resolved(),
            "url" if parent == "image" && nested_feed_field && f.image.is_empty() => {
                f.image = resolved()
            }
            "logo" | "icon" if parent_scope == Scope::Feed => f.image = resolved(),
            _ => {}
        }
    }
    if let Some(parent) = stack.last_mut().filter(|p| {
        !matches!(
            p.name.as_str(),
            "rss" | "channel" | "feed" | "item" | "entry"
        )
    }) && parent.text.len().saturating_add(n.text.len()) <= 1024 * 1024
    {
        parent.text.push_str(&n.text);
    }
}
pub(crate) fn is_apple_url(value: &str) -> bool {
    url(value, None).is_ok_and(|u| {
        matches!(
            u.host_str(),
            Some("podcasts.apple.com" | "itunes.apple.com")
        )
    })
}
pub fn discover(config: &Config, value: &str, stop: &Arc<AtomicBool>) -> Result<Vec<String>> {
    let u = url(value, None)?;
    if matches!(
        u.host_str(),
        Some("podcasts.apple.com" | "itunes.apple.com")
    ) && let Some(id) = u
        .path_segments()
        .and_then(|s| {
            s.filter_map(|p| p.strip_prefix("id").map(str::to_owned))
                .next_back()
        })
        .or_else(|| {
            u.query_pairs()
                .find(|(k, _)| k == "id")
                .map(|(_, v)| v.into_owned())
        })
    {
        let country = u
            .path_segments()
            .and_then(|mut p| p.next())
            .filter(|p| p.len() == 2 && p.bytes().all(|c| c.is_ascii_alphabetic()));
        let mut args = vec![("id", id.as_str())];
        if let Some(country) = country {
            args.push(("country", country));
        }
        return apple(config, "lookup", &args, stop).map(|v| {
            v.into_iter()
                .filter_map(|v| {
                    v["feedUrl"]
                        .as_str()
                        .and_then(|s| url(s, None).ok())
                        .map(|u| u.to_string())
                })
                .collect()
        });
    }
    let (base, data) = body(
        client(config)?
            .get(u.clone())
            .send()
            .map_err(network_error)?,
        stop,
    )?;
    if parse(&data, &base).is_ok() {
        return Ok(vec![u.to_string()]);
    }
    let html = Html::parse_document(&String::from_utf8_lossy(&data));
    let mut found = vec![];
    let base = html
        .select(&Selector::parse("head base[href]").unwrap())
        .next()
        .and_then(|e| url(e.value().attr("href")?, Some(&base)).ok())
        .unwrap_or(base);
    for e in html.select(&Selector::parse("head link[href]").unwrap()) {
        let rel = e.value().attr("rel").unwrap_or("");
        let mime = e.value().attr("type").unwrap_or("").to_ascii_lowercase();
        if rel
            .split_whitespace()
            .any(|r| r.eq_ignore_ascii_case("alternate") || r.eq_ignore_ascii_case("feed"))
            && matches!(
                mime.as_str(),
                "application/rss+xml" | "application/atom+xml"
            )
            && let Some(u) = e
                .value()
                .attr("href")
                .and_then(|s| url(s, Some(&base)).ok())
            && !found.contains(&u.to_string())
        {
            found.push(u.to_string());
        }
    }
    Ok(found)
}
fn apple(
    config: &Config,
    endpoint: &str,
    args: &[(&str, &str)],
    stop: &Arc<AtomicBool>,
) -> Result<Vec<serde_json::Value>> {
    use std::sync::{Mutex, OnceLock};
    use std::time::Instant;
    type Cache = std::collections::HashMap<String, (Instant, Vec<serde_json::Value>)>;
    static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();
    static LAST: Mutex<Option<Instant>> = Mutex::new(None);
    let key = format!("{endpoint}{args:?}");
    let cache = CACHE.get_or_init(Mutex::default);
    if let Some((at, v)) = cache.lock().unwrap().get(&key)
        && at.elapsed() < Duration::from_secs(600)
    {
        return Ok(v.clone());
    }
    {
        let mut last = LAST.lock().unwrap();
        if let Some(at) = *last {
            let wait = (at + Duration::from_secs(3)).saturating_duration_since(Instant::now());
            if !wait.is_zero() {
                return Err(error(&format!(
                    "{} ({}s)",
                    crate::t!("podcast_rate_limited"),
                    wait.as_secs().saturating_add(1)
                )));
            }
        }
        *last = Some(Instant::now());
    }
    let r = client(config)?
        .get(format!("https://itunes.apple.com/{endpoint}"))
        .query(args)
        .send()
        .map_err(network_error)?;
    if r.status() == 429 {
        let seconds = r
            .headers()
            .get("retry-after")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(60)
            .min(3600);
        *LAST.lock().unwrap() = Some(Instant::now() + Duration::from_secs(seconds));
        return Err(error(crate::t!("podcast_rate_limited")));
    }
    let (_, data) = body(r, stop)?;
    let value: serde_json::Value =
        serde_json::from_slice(&data).map_err(|_| error(crate::t!("podcast_invalid_feed")))?;
    let rows = value["results"].as_array().cloned().unwrap_or_default();
    let mut c = cache.lock().unwrap();
    if c.len() > 100 {
        c.clear();
    }
    c.insert(key, (Instant::now(), rows.clone()));
    Ok(rows)
}
pub fn search(
    config: &Config,
    query: &str,
    country: &str,
    limit: usize,
    stop: &Arc<AtomicBool>,
) -> Result<Vec<serde_json::Value>> {
    apple(
        config,
        "search",
        &[
            ("term", query),
            ("media", "podcast"),
            ("entity", "podcast"),
            ("country", country),
            ("limit", &limit.to_string()),
        ],
        stop,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    fn parse_xml(xml: &str) -> Result<Preview> {
        parse(
            xml.as_bytes(),
            &Url::parse("https://example.test/a/feed").unwrap(),
        )
    }
    #[test]
    fn transcript_namespace_aliases_and_relative_urls_are_preserved() {
        let p = parse_xml(r#"<rss xmlns:p="https://podcastindex.org/namespace/1.0" xmlns:other="https://example.test"><channel><title>Show</title><item><title>Episode</title><enclosure url="audio.mp3"/><p:transcript url="captions.vtt" type="text/vtt" language="zh"/><other:transcript url="wrong.srt" type="application/x-subrip"/></item></channel></rss>"#).unwrap();
        assert_eq!(p.episodes[0].transcripts.len(), 1);
        assert!(p.episodes[0].transcripts[0].url.ends_with("/captions.vtt"));
        assert_eq!(p.episodes[0].transcripts[0].language.as_deref(), Some("zh"));
    }

    #[test]
    fn atom_source_metadata_stays_separate_from_entry_identity_and_content() {
        for source_first in [false, true] {
            let source = r#"<source xml:base="https://source.test/">
                <id>shared-source</id><title>Source title</title>
                <updated>2026-02-01T00:00:00Z</updated>
                <link rel="alternate" href="source"/>
                <link rel="enclosure" href="source.mp3" type="audio/mpeg"/>
                <logo>source.jpg</logo><author><name>Source author</name></author>
                </source>"#;
            let entries = ["one", "two"].map(|id| {
                let fields = format!(
                    r#"<id>{id}</id><title>Episode {id}</title>
                    <published>2026-01-01T00:00:00Z</published>
                    <summary>Episode description</summary>
                    <link rel="alternate" href="{id}"/>
                    <link rel="enclosure" href="{id}.mp3" type="audio/mpeg"/>"#
                );
                if source_first {
                    format!("<entry>{source}{fields}</entry>")
                } else {
                    format!("<entry>{fields}{source}</entry>")
                }
            });
            let p = parse_xml(&format!(
                r#"<feed xmlns="http://www.w3.org/2005/Atom">
                <title>Show</title><author><name>Show author</name></author>
                <logo>show.jpg</logo>{}</feed>"#,
                entries.join("")
            ))
            .unwrap();
            assert_eq!(p.feed.author, "Show author");
            assert_eq!(p.feed.image, "https://example.test/a/show.jpg");
            for (episode, id) in p.episodes.iter().zip(["one", "two"]) {
                assert_eq!(episode.guid.as_deref(), Some(id));
                assert_eq!(episode.title, format!("Episode {id}"));
                assert_eq!(episode.description, "Episode description");
                assert_eq!(
                    episode.published.as_deref(),
                    Some("2026-01-01T00:00:00+00:00")
                );
                assert_eq!(episode.website, format!("https://example.test/a/{id}"));
                assert_eq!(episode.url, format!("https://example.test/a/{id}.mp3"));
            }
            let dir = tempfile::tempdir().unwrap();
            let store = crate::store::Store::open(&dir.path().join("radio.db")).unwrap();
            let feed = store.podcast_subscribe(&p, &p.feed.url).unwrap();
            assert_eq!(store.podcast_episodes(&feed.id).unwrap().len(), 2);
        }
    }
    #[test]
    fn rss_episode_images_never_replace_or_supply_show_artwork() {
        for show_image in ["", r#"<itunes:image href="show.jpg"/>"#] {
            for show_first in [false, true] {
                let episodes = r#"<item><title>One</title><enclosure url="one.mp3"/>
                    <itunes:image href="one.jpg"/></item>
                    <item><title>Two</title><enclosure url="two.mp3"/>
                    <itunes:image href="two.jpg"/></item>"#;
                let content = if show_first {
                    format!("{show_image}{episodes}")
                } else {
                    format!("{episodes}{show_image}")
                };
                let p = parse_xml(&format!(
                    r#"<rss xmlns:itunes="http://www.itunes.com/dtds/podcast-1.0.dtd">
                    <channel><title>Show</title>{content}</channel></rss>"#
                ))
                .unwrap();
                assert_eq!(p.episodes.len(), 2);
                assert_eq!(
                    p.feed.image,
                    if show_image.is_empty() {
                        ""
                    } else {
                        "https://example.test/a/show.jpg"
                    }
                );
            }
        }
        let p = parse_xml(
            r#"<rss><channel><title>Show</title><image>
            <title>Image title</title><url>rss.jpg</url><link>https://image.test/</link>
            </image><link>https://show.test/</link>
            <item><title>Episode</title><enclosure url="one.mp3"/></item>
            </channel></rss>"#,
        )
        .unwrap();
        assert_eq!(p.feed.title, "Show");
        assert_eq!(p.feed.website, "https://show.test/");
        assert_eq!(p.feed.image, "https://example.test/a/rss.jpg");
    }
    #[test]
    fn rss_metadata_entities_cdata_and_relative_enclosures() {
        let p=parse_xml(r#"<rss xmlns:itunes="http://www.itunes.com/dtds/podcast-1.0.dtd"><channel><title>节目 &amp; 测试</title><itunes:author>作者</itunes:author><item><title><![CDATA[第 <b>一</b> 集]]></title><guid>opaque</guid><itunes:duration>1:02:03</itunes:duration><enclosure url="../audio.m4a?a=1&amp;b=2" type="audio/mp4" length="123"/></item></channel></rss>"#).unwrap();
        assert_eq!(p.feed.title, "节目 & 测试");
        assert_eq!(p.feed.author, "作者");
        assert_eq!(p.episodes[0].title, "第 一 集");
        assert_eq!(p.episodes[0].duration_ms, Some(3723000));
        assert_eq!(p.episodes[0].bytes, Some(123));
        assert_eq!(p.episodes[0].url, "https://example.test/audio.m4a?a=1&b=2");
    }
    #[test]
    fn atom_enclosures_respect_xml_base_and_unknown_duration() {
        let p=parse_xml(r#"<feed xmlns="http://www.w3.org/2005/Atom" xml:base="https://media.test/"><title>Atom</title><entry><id>x</id><title>one</title><link rel="enclosure" href="one.mp3" type="audio/mpeg"/></entry></feed>"#).unwrap();
        assert_eq!(p.episodes[0].url, "https://media.test/one.mp3");
        assert_eq!(p.episodes[0].duration_ms, None);
    }
    #[test]
    fn atom_publication_dates_prefer_published_independently_of_tag_order() {
        let published = "<published>2026-01-01T00:00:00Z</published>";
        let updated = "<updated>2026-03-01T00:00:00Z</updated>";
        for (dates, expected) in [
            (format!("{published}{updated}"), "2026-01-01T00:00:00+00:00"),
            (format!("{updated}{published}"), "2026-01-01T00:00:00+00:00"),
            (published.into(), "2026-01-01T00:00:00+00:00"),
            (updated.into(), "2026-03-01T00:00:00+00:00"),
            (
                format!("{updated}<published>invalid</published>"),
                "2026-03-01T00:00:00+00:00",
            ),
            (
                format!("<published>invalid</published>{updated}"),
                "2026-03-01T00:00:00+00:00",
            ),
            (
                format!("{published}<updated>invalid</updated>"),
                "2026-01-01T00:00:00+00:00",
            ),
        ] {
            let p = parse_xml(&format!(
                r#"<feed xmlns="http://www.w3.org/2005/Atom"><title>Show</title>
                <entry><id>one</id><title>One</title>{dates}<link rel="enclosure" href="one.mp3"/></entry>
                <entry><id>two</id><title>Two</title><published>2026-02-01T00:00:00Z</published>
                <link rel="enclosure" href="two.mp3"/></entry></feed>"#
            )).unwrap();
            assert_eq!(
                p.episodes[0].published.as_deref(),
                Some(expected),
                "{dates}"
            );
            let dir = tempfile::tempdir().unwrap();
            let store = crate::store::Store::open(&dir.path().join("radio.db")).unwrap();
            let feed = store.podcast_subscribe(&p, &p.feed.url).unwrap();
            let (page, total) = store
                .podcast_episodes_page(
                    &feed.id,
                    "",
                    &crate::cli::Page {
                        offset: 0,
                        limit: 1,
                        all: false,
                    },
                )
                .unwrap();
            assert_eq!(total, 2);
            assert_eq!(
                page[0].guid.as_deref(),
                Some(if expected.contains("01-01") {
                    "two"
                } else {
                    "one"
                })
            );
        }
    }
    #[test]
    fn atom_text_constructs_preserve_literal_markup() {
        for (kind, content) in [
            ("", "Show &lt;test&gt; &amp;lt;literal&amp;gt;"),
            (
                r#" type="text""#,
                "Show &lt;test&gt; &amp;lt;literal&amp;gt;",
            ),
            (r#" type="text""#, "<![CDATA[Show <test> &lt;literal&gt;]]>"),
            (
                r#" type="html""#,
                "&lt;b&gt;Show&lt;/b&gt; &amp;lt;test&amp;gt; &amp;amp;lt;literal&amp;amp;gt;",
            ),
            (
                r#" type="xhtml""#,
                r#"<div xmlns="http://www.w3.org/1999/xhtml"><b>Show</b> &lt;test&gt; &amp;lt;literal&amp;gt;</div>"#,
            ),
        ] {
            let p = parse_xml(&format!(
                r#"<feed xmlns="http://www.w3.org/2005/Atom">
                <title{kind}>{content}</title><subtitle{kind}>{content}</subtitle>
                <entry><id>one</id><title{kind}>{content}</title>
                <summary{kind}>{content}</summary>
                <link rel="enclosure" href="one.mp3" type="audio/mpeg"/>
                </entry></feed>"#,
            ))
            .unwrap();
            let expected = "Show <test> &lt;literal&gt;";
            assert_eq!(p.feed.title, expected, "{kind}: {content}");
            assert_eq!(p.feed.description, expected);
            assert_eq!(p.episodes[0].title, expected);
            assert_eq!(p.episodes[0].description, expected);
        }
    }
    #[test]
    fn malformed_external_entities_and_missing_audio_are_rejected() {
        for xml in [
            "<!DOCTYPE rss SYSTEM 'http://bad.test/dtd'><rss/>",
            "<rss><channel><title>x</title><item><enclosure url='file:///etc/passwd'/></item></channel></rss>",
            "<rss><channel><title>x</title><item><enclosure url='https://example.test/a'/></channel>",
            "<html><title>x</title></html>",
        ] {
            assert!(parse_xml(xml).is_err(), "{xml}");
        }
        let xml = format!("<rss>{}</rss>", "<a>".repeat(65));
        assert!(parse_xml(&xml).is_err());
    }
}

#[cfg(test)]
mod tolerant_date_tests {
    use super::*;
    #[test]
    fn tolerant_dates_preserve_offsets_without_inventing_timezones() {
        for s in [
            "Tue, 01 Jan 2024 10:00:00 +0800",
            "Monday, 01 Jan 2024 10:00:00 GMT",
            "Mon, 01 Jan 2024 10:00:00 UTC",
            "Mon, 01 Jan 2024 10:00:00 CEST",
            "Mon, 01 Jan 2024 10:00:00 +08:00",
        ] {
            assert!(published(s).is_some(), "{s}");
        }
        assert!(published("2024-01-01T10:00:00").is_none());
        assert_eq!(duration("01:02.5"), Some(62500));
        assert_eq!(duration("NaN"), None);
        assert_eq!(duration("-1"), None);
    }
}

#[cfg(test)]
mod chapter_tests {
    use super::*;
    #[test]
    fn chapters_and_ogg_use_their_declared_namespaces_and_media_times() {
        let p = parse(br#"<rss xmlns:p="https://podcastindex.org/namespace/1.0" xmlns:c="http://podlove.org/simple-chapters"><channel><title>Show</title><item><title>Episode</title><enclosure url="audio.ogg" type="application/ogg"/><p:chapters url="chapters.json" type="application/json+chapters"/><c:chapters><c:chapter start="00:01.5" title="Intro"/></c:chapters></item></channel></rss>"#, &Url::parse("https://chapters.test/feed").unwrap()).unwrap();
        assert_eq!(p.episodes[0].chapters[0].position_ms, 1500);
        assert_eq!(
            p.episodes[0].chapters_url.as_deref(),
            Some("https://chapters.test/chapters.json")
        );
    }
}
