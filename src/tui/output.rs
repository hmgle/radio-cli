use std::io::{self, Write};

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
