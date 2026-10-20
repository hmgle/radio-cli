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

pub(crate) fn display_zh(code: &str) -> String {
    let code = code.trim().to_ascii_uppercase();
    match names().get(&code) {
        Some(name) => format!("{name} ({code})"),
        None => code,
    }
}

pub(crate) fn matching_codes(query: &str) -> impl Iterator<Item = &'static str> + '_ {
    names()
        .iter()
        .filter(move |(_, name)| name.contains(query))
        .map(|(code, _)| code.as_str())
}
