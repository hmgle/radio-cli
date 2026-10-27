//! Optional presentation rules for the damaged AsiaFM Cantonese metadata.
use crate::config::Settings;

const STATION_UUID: &str = "fb1a81c7-cdf2-4ed8-912c-796918d0fa08";

pub(crate) fn enabled(settings: &Settings, stationuuid: &str, url: &str) -> bool {
    settings.asiafm_metadata_patch
        && (stationuuid == STATION_UUID
            || reqwest::Url::parse(url).is_ok_and(|url| {
                matches!(url.scheme(), "http" | "https")
                    && url.host_str() == Some("yyt.asiafm.net")
                    && url.port_or_known_default() == Some(8000)
                    && url.path() == "/asiafm"
            }))
}

pub(crate) fn title(title: &str) -> String {
    // Match only the observed fixed watermark, including its incomplete 台.
    // It identifies the station, not the song, and is redundant in the UI.
    // An unknown suffix is left untouched in case the upstream format changes.
    let Some(body) = [
        "(AsiaFM亚洲粤语台)",
        "(AsiaFM亚洲粤语�?)",
        "(AsiaFM亚洲粤语�)",
    ]
    .iter()
    .find_map(|suffix| title.trim_end().strip_suffix(suffix)) else {
        return title.to_owned();
    };
    // Normalize every �- as a damaged separator; an intentional occurrence
    // is ambiguous and receives the same treatment within this opt-in patch.
    let body = body.replace("�?", "�").replace("�-", "� -");
    // A whole field containing only a Latin-1 UTF-8 lead is a known AsiaFM
    // truncation form (e.g. E7 -> ç). Limit this opt-in heuristic to whole
    // fields and E0-F4 (three/four-byte leads) so actual words such as Café
    // and Jóga remain intact; ambiguous two-byte leads C2-DF are excluded.
    let fields: Vec<_> = body
        .split(" - ")
        .map(|field| {
            let field = field.trim();
            let lead = field.strip_suffix('?').unwrap_or(field);
            if lead.chars().count() == 1 && lead.chars().all(|c| matches!(c, '\u{e0}'..='\u{f4}')) {
                "�"
            } else {
                field
            }
        })
        .collect();
    // Never guess artists or songs, remove missing fields, or promote the
    // watermark to a track title when there is no actual track information.
    fields
        .join(" - ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requires_opt_in_and_the_station_uuid_or_exact_stream_endpoint() {
        let mut settings = Settings::default();
        let url = "http://yyt.asiafm.net:8000/asiafm";
        assert!(!enabled(&settings, STATION_UUID, url));
        settings.asiafm_metadata_patch = true;
        assert!(enabled(
            &settings,
            STATION_UUID,
            "http://127.0.0.1:1234/live"
        ));
        assert!(enabled(&settings, "custom", url));
        assert!(enabled(
            &settings,
            "",
            "https://yyt.asiafm.net:8000/asiafm?x=1"
        ));
        for url in [
            "http://yyt.asiafm.net.evil.test:8000/asiafm",
            "http://yyt.asiafm.net:8000/other",
            "http://yyt.asiafm.net/asiafm",
            "file:///asiafm",
            "not a URL",
        ] {
            assert!(!enabled(&settings, "other", url), "{url}");
        }
    }

    #[test]
    fn removes_only_the_known_watermark_and_keeps_missing_information_visible() {
        for (input, expected) in [
            ("张国荣  (AsiaFM亚洲粤语�?)", "张国荣"),
            ("彭羚 - 愿你 (AsiaFM亚洲粤语台)", "彭羚 - 愿你"),
            ("陈小�?- ? (AsiaFM亚洲粤语�?)", "陈小� - ?"),
            ("陈奕�? - 爱�(AsiaFM亚洲粤语�?)", "陈奕� - 爱�"),
            ("蔡枫�?- ç (AsiaFM亚洲粤语�?)", "蔡枫� - �"),
            ("关淑�? - 这�? (AsiaFM亚洲粤语�?)", "关淑� - 这�"),
            ("许志�?_ 车婉� (AsiaFM亚洲粤语�?)", "许志�_ 车婉�"),
            ("蒋丽�?- � (AsiaFM亚洲粤语�?)", "蒋丽� - �"),
            ("Björk - Jóga (AsiaFM亚洲粤语台)", "Björk - Jóga"),
            ("Café - 中文é? (AsiaFM亚洲粤语台)", "Café - 中文é?"),
            ("(AsiaFM亚洲粤语�)", ""),
            ("", ""),
            ("Another station - Café?", "Another station - Café?"),
            ("Title (AsiaFM亚洲音乐台)", "Title (AsiaFM亚洲音乐台)"),
            (
                "Title (AsiaFM亚洲粤语unexpected)",
                "Title (AsiaFM亚洲粤语unexpected)",
            ),
        ] {
            assert_eq!(title(input), expected, "{input}");
        }
    }
}
