//! Extract track text from the RadioInfo documents used as HLS/ID3 titles.
use quick_xml::{Reader, events::Event};

// Parse the complete document before the display limit is applied, while
// bounding work on metadata supplied by an external stream.
const XML_LIMIT: usize = 64 * 1024;
const DEPTH_LIMIT: usize = 16;

#[derive(Default)]
struct Track {
    artist: String,
    title: String,
    lead_artist: String,
    song: String,
}

impl Track {
    fn field(&mut self, name: &str, text: &str) {
        let target = match name {
            "DB_DALET_ARTIST_NAME" => &mut self.artist,
            "DB_DALET_TITLE_NAME" => &mut self.title,
            "DB_LEAD_ARTIST_NAME" => &mut self.lead_artist,
            "DB_SONG_NAME" => &mut self.song,
            _ => return,
        };
        if target.is_empty() {
            *target = text.split_whitespace().collect::<Vec<_>>().join(" ");
        }
    }

    fn display(self) -> String {
        let artist = if self.artist.is_empty() {
            self.lead_artist
        } else {
            self.artist
        };
        let title = if self.title.is_empty() {
            self.song
        } else {
            self.title
        };
        match (artist.is_empty(), title.is_empty()) {
            (false, false) => format!("{artist} - {title}"),
            (false, true) => artist,
            _ => title,
        }
    }
}

/// Unknown, malformed or excessive documents keep the normal text fallback.
/// A valid RadioInfo document without track fields explicitly clears the title.
/// Select the first nonempty direct Table in document order. Later records
/// cannot complete a partial title from an earlier record.
pub(super) fn title(text: &str) -> Option<String> {
    let xml = text.trim().trim_start_matches('\u{feff}').trim_start();
    if !xml.starts_with('<') || xml.len() > XML_LIMIT {
        return None;
    }
    let mut reader = Reader::from_str(xml);
    reader.config_mut().expand_empty_elements = true;
    let mut stack: Vec<(String, String)> = Vec::new();
    let mut root = false;
    let mut track: Option<Track> = None;
    let mut selected = None;
    loop {
        match reader.read_event().ok()? {
            Event::Start(element) => {
                let name = element.name().as_ref().to_owned();
                if stack.is_empty() {
                    if root || name != "RadioInfo" {
                        return None;
                    }
                    root = true;
                }
                if stack.len() >= DEPTH_LIMIT {
                    return None;
                }
                if stack.len() == 1 && name == "Table" {
                    track = Some(Track::default());
                }
                stack.push((name, String::new()));
            }
            Event::End(_) => {
                let (name, value) = stack.pop()?;
                // Programme titles, hosts and unrelated nested fields must
                // never be mistaken for the current song.
                if stack.len() == 2 && stack[1].0 == "Table" {
                    track.as_mut()?.field(&name, &value);
                }
                if stack.len() == 1 && name == "Table" {
                    let title = track.take()?.display();
                    if selected.is_none() && !title.is_empty() {
                        selected = Some(title);
                    }
                }
            }
            Event::Text(value) => {
                if let Some((_, text)) = stack.last_mut() {
                    text.push_str(value.as_ref());
                } else if !value.trim().is_empty() {
                    return None;
                }
            }
            Event::CData(value) => stack.last_mut()?.1.push_str(value.as_ref()),
            Event::GeneralRef(value) => {
                let reference = format!("&{};", value.as_ref());
                let decoded = quick_xml::escape::unescape(&reference).ok()?;
                stack.last_mut()?.1.push_str(&decoded);
            }
            Event::DocType(_) => return None,
            Event::Eof => break,
            _ => {}
        }
    }
    (root && stack.is_empty()).then(|| selected.unwrap_or_default())
}
