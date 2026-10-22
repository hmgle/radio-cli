//! Finds credentials embedded in URLs, so they can be kept out of places the
//! user did not ask to store them in.

use std::ops::Range;

const SECRET_WORDS: [&str; 7] = [
    "token",
    "key",
    "secret",
    "password",
    "passwd",
    "signature",
    "sig",
];
const SECRET_NAMES: [&str; 4] = ["auth", "authorization", "pass", "pwd"];

/// Whether a query parameter name usually carries a secret, such as `token`,
/// `access_token`, `api-key` or `X-Amz-Signature`.
fn secret_name(name: &str) -> bool {
    // Query names may be percent-encoded. Decode through the URL parser while
    // keeping the ranges below relative to the original, unmodified text.
    let mut url = reqwest::Url::parse("http://localhost/").unwrap();
    url.set_query(Some(name));
    let name = url
        .query_pairs()
        .next()
        .map(|(name, _)| name.into_owned())
        .unwrap_or_default()
        .to_ascii_lowercase()
        .replace('-', "_");
    SECRET_NAMES.contains(&name.as_str())
        || name.ends_with("apikey")
        || SECRET_WORDS
            .iter()
            .any(|word| name == *word || name.strip_suffix(word).is_some_and(|p| p.ends_with('_')))
}

/// Byte ranges of the credentials inside every URL of `text`: the user info of
/// the authority and the values of secret-looking query parameters.
pub(crate) fn credential_ranges(text: &str) -> Vec<Range<usize>> {
    let mut ranges = Vec::new();
    let mut from = 0;
    while let Some(found) = text[from..].find("://") {
        let marker = from + found;
        let start = marker + 3;
        let end = text[start..]
            .find(|c: char| {
                c.is_whitespace() || c.is_control() || matches!(c, '\'' | '"' | '<' | '>')
            })
            .map_or(text.len(), |i| start + i);
        // Keep looking inside the token too: a redirect URL may carry a URL
        // with its own credentials in a query parameter.
        from = start;
        if !text[..marker]
            .chars()
            .next_back()
            .is_some_and(|c| c.is_ascii_alphanumeric())
        {
            continue;
        }
        let url = &text[start..end];
        let authority = url.find(['/', '?', '#']).unwrap_or(url.len());
        if let Some(at) = url[..authority].rfind('@')
            && at > 0
        {
            ranges.push(start..start + at);
        }
        let before_fragment = url.find('#').unwrap_or(url.len());
        let Some(question) = url[..before_fragment].find('?') else {
            continue;
        };
        let mut position = question + 1;
        for pair in url[position..before_fragment].split('&') {
            if let Some((name, value)) = pair.split_once('=')
                && !value.is_empty()
                && secret_name(name)
            {
                let value_start = start + position + name.len() + 1;
                ranges.push(value_start..value_start + value.len());
            }
            position += pair.len() + 1;
        }
    }
    ranges
}

/// Whether `text` contains a URL with user info or a secret query parameter.
pub(crate) fn has_url_credentials(text: &str) -> bool {
    !credential_ranges(text).is_empty()
}

/// Redact only URL credentials, retaining useful hosts, paths and parameter
/// names. Merge overlapping ranges from nested URLs before replacing them.
pub(crate) fn urls(text: &str) -> String {
    let mut ranges = credential_ranges(text);
    ranges.sort_by_key(|range| range.start);
    let mut merged: Vec<Range<usize>> = Vec::new();
    for range in ranges {
        if let Some(last) = merged.last_mut()
            && range.start <= last.end
        {
            last.end = last.end.max(range.end);
        } else {
            merged.push(range);
        }
    }
    let mut result = String::new();
    let mut from = 0;
    for range in merged {
        result.push_str(&text[from..range.start]);
        result.push_str("[redacted]");
        from = range.end;
    }
    result.push_str(&text[from..]);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secrets(text: &str) -> Vec<&str> {
        credential_ranges(text)
            .into_iter()
            .map(|range| &text[range])
            .collect()
    }

    #[test]
    fn user_info_is_found_with_or_without_a_password() {
        assert_eq!(secrets("https://user:pa55@example.com/live"), ["user:pa55"]);
        assert_eq!(secrets("http://admin@example.com:8000/"), ["admin"]);
        // The password may itself contain an @.
        assert_eq!(secrets("https://u:p@ss@host/x"), ["u:p@ss"]);
        assert_eq!(secrets("station add 'A' 'ftp://me:pw@h/s' --x"), ["me:pw"]);
    }

    #[test]
    fn secret_query_values_are_found_by_name() {
        for name in [
            "token",
            "TOKEN",
            "access_token",
            "api-key",
            "apikey",
            "X-Amz-Signature",
            "key",
            "sig",
            "password",
            "auth",
            "%74oken",
            "access%5Ftoken",
        ] {
            let text = format!("https://example.com/live?quality=high&{name}=abc123&x=1");
            assert_eq!(secrets(&text), ["abc123"], "{name}");
        }
    }

    #[test]
    fn plain_urls_and_text_are_left_alone() {
        for text in [
            "",
            "station add Name https://example.com/live",
            "https://example.com/live?quality=high&lang=fr&keyboard=1&monkey=2",
            "https://example.com/live?token=",
            "https://example.com/live#frag?token=abc",
            "https://example.com/user@name",
            "https://example.com/p?next=https://example.org/@x",
            "mail me@example.com about token=abc",
            "search 咖啡 --country FR",
            "https://@example.com/",
        ] {
            assert!(secrets(text).is_empty(), "{text}");
            assert!(!has_url_credentials(text), "{text}");
        }
    }

    #[test]
    fn every_url_of_a_line_is_checked_and_ranges_stay_on_char_boundaries() {
        let text = "添加 https://电台.example/live?key=一二三 然后 http://a:b@h/x?token=z";
        assert_eq!(secrets(text), ["一二三", "a:b", "z"]);
        assert!(has_url_credentials(text));
        assert!(has_url_credentials(
            "https://example.com/?next=https://u:p@host/live"
        ));
    }

    #[test]
    fn redaction_preserves_context_and_handles_overlapping_nested_urls() {
        assert_eq!(
            urls("失败 https://u:pw@host/live?quality=1&%74oken=秘密\nretry"),
            "失败 https://[redacted]@host/live?quality=1&%74oken=[redacted]\nretry"
        );
        assert_eq!(
            urls("https://host/?token=https://u:pw@nested/?key=secret"),
            "https://host/?token=[redacted]"
        );
    }
}
