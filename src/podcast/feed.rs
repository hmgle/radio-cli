use super::*;
use crate::config::Config;
use anyhow::Result;
use quick_xml::{Reader, events::Event};
use reqwest::{
    Url,
    blocking::{Client, Response},
};
use scraper::{Html, Selector};
use std::{
    io::Read,
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
        .map_err(|_| error(crate::t!("podcast_network")))
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
            .map_err(|_| error(crate::t!("podcast_network")))?;
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
    let u = url(value, None)?;
    let mut req = client(config)?.get(u);
    if let Some(f) = old {
        if let Some(v) = &f.etag {
            req = req.header("If-None-Match", v);
        }
        if let Some(v) = &f.modified {
            req = req.header("If-Modified-Since", v);
        }
    }
    let r = req
        .send()
        .map_err(|_| error(crate::t!("podcast_network")))?;
    if r.status() == 304 && old.is_some() {
        return Ok(None);
    }
    let etag = r
        .headers()
        .get("etag")
        .and_then(|h| h.to_str().ok())
        .map(str::to_owned);
    let modified = r
        .headers()
        .get("last-modified")
        .and_then(|h| h.to_str().ok())
        .map(str::to_owned);
    let (base, data) = body(r, stop)?;
    let mut p = parse(&data, &base)?;
    p.feed.etag = etag;
    p.feed.modified = modified;
    p.feed.refreshed_at = Some(chrono::Utc::now().to_rfc3339());
    Ok(Some(p))
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
fn duration(value: &str) -> Option<u64> {
    let mut n = 0u64;
    for part in value.trim().split(':') {
        n = n.checked_mul(60)?.checked_add(part.parse::<u64>().ok()?)?;
    }
    n.checked_mul(1000)
}
struct Node {
    name: String,
    text: String,
    base: Url,
    html: bool,
}
pub(crate) fn parse(data: &[u8], base: &Url) -> Result<Preview> {
    if data.len() as u64 > LIMIT {
        return Err(error(crate::t!("podcast_limit")));
    }
    if data.starts_with(&[0xff, 0xfe]) || data.starts_with(&[0xfe, 0xff]) {
        return Err(error(crate::t!("podcast_invalid_feed")));
    }
    let invalid = || error(crate::t!("podcast_invalid_feed"));
    let mut reader = Reader::from_reader(data);
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
                if matches!(name.as_str(), "item" | "entry") {
                    count += 1;
                    if count > 10000 || current.is_some() {
                        return Err(error(crate::t!("podcast_limit")));
                    }
                    current = Some(Episode::default());
                }
                if name == "enclosure"
                    || (name == "link" && attrs.get("rel").is_some_and(|r| r == "enclosure"))
                {
                    if let Some(ep) = current.as_mut() {
                        let mime = attrs.get("type").cloned().unwrap_or_default();
                        if ep.url.is_empty()
                            && (mime.is_empty()
                                || mime.starts_with("audio/")
                                || mime == "application/octet-stream")
                            && let Some(u) = attr_url(if name == "link" { "href" } else { "url" })
                        {
                            ep.url = u;
                            ep.mime = mime;
                            ep.bytes = attrs.get("length").and_then(|v| v.parse().ok());
                        }
                    }
                } else if name == "image"
                    && let Some(u) = attr_url("href")
                {
                    f.image = u;
                } else if name == "link"
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
    let resolved = || {
        url(text, Some(&n.base))
            .map(|u| u.to_string())
            .unwrap_or_default()
    };
    if let Some(e) = current.as_mut() {
        match n.name.as_str() {
            "title" => e.title = display_text(),
            "guid" | "id" if !text.is_empty() => e.guid = Some(text.into()),
            "description" | "summary" | "content" => e.description = display_text(),
            "pubDate" | "published" | "updated" => {
                e.published = chrono::DateTime::parse_from_rfc2822(text)
                    .or_else(|_| chrono::DateTime::parse_from_rfc3339(text))
                    .ok()
                    .map(|d| d.to_utc().to_rfc3339())
            }
            "link" if !text.is_empty() => e.website = resolved(),
            "duration" => e.duration_ms = duration(text),
            _ => {}
        }
        if matches!(n.name.as_str(), "item" | "entry") {
            let e = current.take().unwrap();
            if !e.url.is_empty() {
                episodes.push(e);
            }
        }
    } else {
        match n.name.as_str() {
            "title" if matches!(parent, "channel" | "feed") => f.title = display_text(),
            "author" | "managingEditor" => f.author = display_text(),
            "name" if parent == "author" => f.author = display_text(),
            "description" | "subtitle" => f.description = display_text(),
            "link" if !text.is_empty() => f.website = resolved(),
            "url" if parent == "image" && f.image.is_empty() => f.image = resolved(),
            "logo" | "icon" => f.image = resolved(),
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
        return apple(config, "lookup", &[("id", &id)], stop).map(|v| {
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
            .get(u)
            .send()
            .map_err(|_| error(crate::t!("podcast_network")))?,
        stop,
    )?;
    if parse(&data, &base).is_ok() {
        return Ok(vec![base.to_string()]);
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
        while last.is_some_and(|at| at.elapsed() < Duration::from_secs(3)) {
            if stop.load(Ordering::Relaxed) {
                return Err(DomainError::new(ErrorCode::Cancelled).into());
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        *last = Some(Instant::now());
    }
    let r = client(config)?
        .get(format!("https://itunes.apple.com/{endpoint}"))
        .query(args)
        .send()
        .map_err(|_| error(crate::t!("podcast_network")))?;
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
