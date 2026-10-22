use std::io::{self, Write};

pub(super) const CLIPBOARD_LIMIT: usize = 4096;

pub(super) fn clipboard_truncated(value: &str) -> bool {
    value.len() > CLIPBOARD_LIMIT
}

pub(super) fn clipboard_sequence(value: &str) -> Vec<u8> {
    use base64::Engine;
    let mut end = value.len().min(CLIPBOARD_LIMIT);
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    let encoded = base64::engine::general_purpose::STANDARD.encode(&value.as_bytes()[..end]);
    format!("\x1b]52;c;{encoded}\x07").into_bytes()
}

/// Remove SGR commands, including backend resets, while preserving cursor and
/// screen control. Escape sequences can span separate Write calls.
pub(super) struct TerminalWriter<W> {
    inner: W,
    strip_sgr: bool,
    pending: Vec<u8>,
}

impl<W> TerminalWriter<W> {
    pub fn new(inner: W, strip_sgr: bool) -> Self {
        Self {
            inner,
            strip_sgr,
            pending: Vec::new(),
        }
    }
}

impl<W: Write> Write for TerminalWriter<W> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if !self.strip_sgr {
            return self.inner.write(bytes);
        }
        let mut output = Vec::with_capacity(bytes.len());
        for &byte in bytes {
            if self.pending.is_empty() {
                if byte == 0x1b {
                    self.pending.push(byte);
                } else {
                    output.push(byte);
                }
                continue;
            }
            self.pending.push(byte);
            let non_csi = self.pending.len() == 2 && byte != b'[';
            let complete = self.pending.len() >= 3 && (0x40..=0x7e).contains(&byte);
            if non_csi || complete || self.pending.len() > 128 {
                if !complete || byte != b'm' {
                    output.extend_from_slice(&self.pending);
                }
                self.pending.clear();
            }
        }
        self.inner.write_all(&output)?;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn clipboard_payload_is_bounded_utf8_and_cannot_inject_controls() {
        use base64::Engine;
        let value = format!("\x1b]injection\x07{}", "中文".repeat(2000));
        let bytes = clipboard_sequence(&value);
        let encoded = &bytes[b"\x1b]52;c;".len()..bytes.len() - 1];
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .unwrap();
        assert!(decoded.len() <= 4096);
        assert!(std::str::from_utf8(&decoded).is_ok());
        assert!(!encoded.contains(&0x1b));
    }
    #[test]
    fn strips_fragmented_colors_and_resets_but_preserves_screen_controls() {
        let mut writer = TerminalWriter::new(Vec::new(), true);
        for part in ["\x1b", "[38;5;1", "m红", "色\x1b[0m\x1b[2J\x1b[?25l"] {
            writer.write_all(part.as_bytes()).unwrap();
            writer.flush().unwrap();
        }
        assert_eq!(
            String::from_utf8(writer.inner).unwrap(),
            "红色\x1b[2J\x1b[?25l"
        );
    }
}
