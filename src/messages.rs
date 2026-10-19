use std::sync::{
    OnceLock,
    atomic::{AtomicU8, Ordering},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Locale {
    En,
    ZhCn,
}

static LOCALE: AtomicU8 = AtomicU8::new(0);
static UNKNOWN_REPORTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn select(
    explicit: Option<&str>,
    environment: Option<&str>,
    config: Option<&str>,
    lc_all: Option<&str>,
    lang: Option<&str>,
) -> (Locale, Option<String>) {
    let value = [explicit, environment, config, lc_all, lang]
        .into_iter()
        .flatten()
        .find(|s| !s.trim().is_empty())
        .unwrap_or("en");
    let normalized = value.trim().to_ascii_lowercase().replace('_', "-");
    let base = normalized.split(['.', '@']).next().unwrap_or("en");
    match base {
        "en" | "c" | "posix" => (Locale::En, None),
        "zh" | "zh-cn" | "zh-hans" => (Locale::ZhCn, None),
        s if s.starts_with("en-") => (Locale::En, None),
        _ => (Locale::En, Some(value.into())),
    }
}

pub fn init(explicit: Option<&str>, config: Option<&str>) {
    configure(explicit, config, true);
}

pub(crate) fn preview(explicit: Option<&str>) {
    configure(explicit, None, false);
}

fn configure(explicit: Option<&str>, config: Option<&str>, report_unknown: bool) {
    let environment = std::env::var("RADIO_LANG").ok();
    let lc_all = std::env::var("LC_ALL").ok();
    let lang = std::env::var("LANG").ok();
    let (locale, unknown) = select(
        explicit,
        environment.as_deref(),
        config,
        lc_all.as_deref(),
        lang.as_deref(),
    );
    LOCALE.store(u8::from(locale == Locale::ZhCn), Ordering::Relaxed);
    if report_unknown
        && let Some(unknown) = unknown
        && !UNKNOWN_REPORTED.swap(true, Ordering::Relaxed)
    {
        eprintln!(
            "radio: unsupported language '{}'; using English",
            crate::clean_text(&unknown)
        );
    }
}

pub fn locale() -> Locale {
    if LOCALE.load(Ordering::Relaxed) == 1 {
        Locale::ZhCn
    } else {
        Locale::En
    }
}

pub fn text(key: &str) -> &'static str {
    text_in(locale(), key)
}

pub fn text_in(locale: Locale, key: &str) -> &'static str {
    static EN: OnceLock<toml::Table> = OnceLock::new();
    static ZH: OnceLock<toml::Table> = OnceLock::new();
    let table = match locale {
        Locale::En => EN.get_or_init(|| {
            toml::from_str(include_str!("../locales/en.toml")).expect("embedded English messages")
        }),
        Locale::ZhCn => ZH.get_or_init(|| {
            toml::from_str(include_str!("../locales/zh-CN.toml"))
                .expect("embedded Chinese messages")
        }),
    };
    table
        .get(key)
        .and_then(toml::Value::as_str)
        .unwrap_or("Unknown message")
}

#[macro_export]
macro_rules! t {
    ($key:literal) => {
        $crate::messages::text($key)
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ErrorCode;
    use std::collections::BTreeSet;

    #[test]
    fn locale_precedence_and_fallback_are_deterministic() {
        assert_eq!(
            select(Some("zh-CN"), Some("en"), None, None, None).0,
            Locale::ZhCn
        );
        assert_eq!(
            select(None, Some("en"), Some("zh-CN"), None, None).0,
            Locale::En
        );
        assert_eq!(
            select(None, None, Some("zh-CN"), Some("en"), None).0,
            Locale::ZhCn
        );
        assert_eq!(
            select(None, None, None, Some("zh_CN.UTF-8"), Some("en")).0,
            Locale::ZhCn
        );
        assert_eq!(
            select(None, None, None, Some(""), Some("zh_CN.UTF-8")).0,
            Locale::ZhCn
        );
        assert_eq!(
            select(Some("fr"), None, Some("zh-CN"), None, None),
            (Locale::En, Some("fr".into()))
        );
        assert_eq!(select(None, None, None, None, None), (Locale::En, None));
    }

    #[test]
    fn catalogs_have_exactly_the_used_keys_and_matching_placeholders() {
        let en: toml::Table = toml::from_str(include_str!("../locales/en.toml")).unwrap();
        let zh: toml::Table = toml::from_str(include_str!("../locales/zh-CN.toml")).unwrap();
        let mut used: BTreeSet<_> = ErrorCode::ALL.iter().map(|c| c.as_str()).collect();
        for part in concat!(include_str!("cli.rs"), include_str!("engine/recording.rs"))
            .split("crate::t!(\"")
            .skip(1)
        {
            used.insert(part.split('"').next().unwrap());
        }
        assert_eq!(en.keys().map(String::as_str).collect::<BTreeSet<_>>(), used);
        assert_eq!(zh.keys().map(String::as_str).collect::<BTreeSet<_>>(), used);
        let placeholders = |s: &str| {
            s.split('{')
                .skip(1)
                .map(|p| p.split('}').next().unwrap().to_string())
                .collect::<BTreeSet<_>>()
        };
        for key in used {
            let a = text_in(Locale::En, key);
            let b = text_in(Locale::ZhCn, key);
            assert!(!a.is_empty() && !b.is_empty());
            assert_eq!(placeholders(a), placeholders(b), "{key}");
        }
    }
}
