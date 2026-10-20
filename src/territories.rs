//! Chinese display names from Unicode CLDR 48, regenerated with
//! `uv run scripts/update_territories.py`. See locales/LICENSE.unicode.
//! This table contains display names only, not station catalog records.

use std::{collections::BTreeMap, sync::OnceLock};

fn names() -> &'static BTreeMap<String, String> {
    static NAMES: OnceLock<BTreeMap<String, String>> = OnceLock::new();
    NAMES.get_or_init(|| {
        serde_json::from_str(include_str!("../locales/territories.zh-CN.json"))
            .expect("embedded CLDR Chinese territory names")
    })
}

fn name_zh(code: &str) -> Option<&'static str> {
    let code = code.trim().to_ascii_uppercase();
    names().get(code.as_str()).map(String::as_str)
}

/// `name (CODE)` for known codes and the bare code otherwise.
pub(crate) fn display_zh(code: &str) -> String {
    let code = code.trim().to_ascii_uppercase();
    match name_zh(&code) {
        Some(name) => format!("{name} ({code})"),
        None => code,
    }
}

/// Localized label that keeps the upstream catalog name when the code is
/// missing or absent from the CLDR table, so no catalog metadata is dropped.
pub(crate) fn display_zh_or(code: &str, upstream: &str) -> String {
    let code = code.trim().to_ascii_uppercase();
    if let Some(name) = name_zh(&code) {
        return format!("{name} ({code})");
    }
    let upstream = upstream.trim();
    if upstream.is_empty() {
        return code;
    }
    if code.is_empty() {
        return upstream.to_owned();
    }
    format!("{upstream} ({code})")
}

pub(crate) fn matching_codes(query: &str) -> impl Iterator<Item = &'static str> + '_ {
    // A single character is too ambiguous to widen a station-name search:
    // "岛" alone matches 32 territories and "国" matches 15.
    let query = query.trim();
    let eligible = query.chars().count() >= 2;
    names()
        .iter()
        .filter(move |(_, name)| eligible && name.contains(query))
        .map(|(code, _)| code.as_str())
}
