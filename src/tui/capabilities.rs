//! Query once before event polling. Older image pickers leave a detached stdin
//! reader behind on timeout, so own the deadline and the only read here.
use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;
use ratatui_image::{
    FontSize, Resize, ResizeEncodeRender,
    picker::{
        Picker, ProtocolType,
        cap_parser::{Parser, QueryStdioOptions, Response},
    },
    protocol::{ImageSource, Protocol, StatefulProtocol, StatefulProtocolType},
};
use std::{
    collections::VecDeque,
    io::{self, Write},
    os::fd::AsRawFd,
    process::Command,
    sync::{Arc, atomic::AtomicBool},
    time::{Duration, Instant},
};

// Allow slower SSH round trips while bounding the first-frame delay on silent
// terminals. DSR ends the wait early; user keys are queued throughout probing.
const QUERY_TIMEOUT: Duration = Duration::from_millis(200);

#[derive(Clone)]
pub(super) enum ImagePicker {
    Terminal(Picker),
    // None retains half blocks without invoking Picker's tmux detection:
    // even Picker::halfblocks() enables allow-passthrough in version 10.0.1.
    Tmux(Option<FontSize>),
}

impl ImagePicker {
    pub(super) fn new_protocol(
        &self,
        image: image::DynamicImage,
        size: Rect,
    ) -> Result<Protocol, ratatui_image::errors::Errors> {
        let resize = Resize::Scale(None);
        let font = match self {
            Self::Terminal(picker) => return picker.new_protocol(image, size, resize),
            Self::Tmux(font) => font,
        };
        let font_size = font.unwrap_or((10, 20));
        let source = ImageSource::new(image, font_size, image::Rgba([0, 0, 0, 0]));
        let area = resize.render_area(&source, font_size, size);
        let kind = if font.is_some() {
            // The default Sixel backend has is_tmux=false: tmux parses raw
            // Sixel itself, including when allow-passthrough is off.
            StatefulProtocolType::Sixel(Default::default())
        } else {
            StatefulProtocolType::Halfblocks(Default::default())
        };
        let mut state = StatefulProtocol::new(source, font_size, kind);
        state.resize_encode(&resize, area);
        if let Some(result) = state.last_encoding_result() {
            result?;
        }
        Ok(match state.protocol_type_owned() {
            StatefulProtocolType::Sixel(sixel) => Protocol::Sixel(sixel),
            StatefulProtocolType::Halfblocks(blocks) => Protocol::Halfblocks(blocks),
            _ => unreachable!(),
        })
    }
}

pub(super) fn picker(pending: &mut VecDeque<Event>) -> ImagePicker {
    if std::env::var_os("TMUX").is_some() {
        let query = || {
            let mut command = Command::new("tmux");
            command.args([
                "display-message",
                "-p",
                concat!(
                    "#{client_name}\t#{client_cell_width}\t#{client_cell_height}\t",
                    "#{sixel_support}\t#{client_termfeatures}",
                ),
            ]);
            let bytes = crate::process::output(
                &mut command,
                Duration::from_millis(250),
                &Arc::new(AtomicBool::new(false)),
            )
            .ok()?;
            String::from_utf8(bytes).ok()
        };
        // picker() runs only at startup. Retry one failed command, but accept
        // a valid negative capability report without retrying. The two attempts
        // have a combined 500 ms timeout budget.
        let font = query().or_else(query).and_then(|report| tmux_font(&report));
        return ImagePicker::Tmux(font);
    }
    let program = std::env::var("TERM_PROGRAM").unwrap_or_default();
    let iterm = iterm2(&program, &std::env::var("LC_TERMINAL").unwrap_or_default());
    ImagePicker::Terminal(
        query(
            &mut io::stdin().lock(),
            &mut io::stdout().lock(),
            QUERY_TIMEOUT,
            iterm,
            pending,
        )
        .unwrap_or_else(|_| Picker::halfblocks()),
    )
}

fn tmux_font(report: &str) -> Option<FontSize> {
    // A pane's DA1 says nothing about the attached terminal. Require the
    // server's build flag (tmux 3.6+) and the client's applied Sixel feature;
    // older servers cannot reliably report whether Sixel was compiled in.
    let fields: Vec<_> = report.trim_end_matches(['\r', '\n']).split('\t').collect();
    let [client, width, height, support, features] = fields.as_slice() else {
        return None;
    };
    if client.is_empty()
        || *support != "1"
        || !features.split(',').any(|feature| feature == "sixel")
    {
        return None;
    }
    let font = (width.parse().ok()?, height.parse().ok()?);
    if !(1..=128).contains(&font.0) || !(1..=256).contains(&font.1) {
        return None;
    }
    Some(font)
}

fn query(
    input: &mut impl AsRawFd,
    output: &mut (impl Write + AsRawFd),
    timeout: Duration,
    iterm: bool,
    pending: &mut VecDeque<Event>,
) -> io::Result<Picker> {
    output.write_all(Parser::query(false, QueryStdioOptions::default()).as_bytes())?;
    output.flush()?;
    let deadline = Instant::now() + timeout;
    let mut parser = Parser::new();
    let mut protocol = ProtocolType::Halfblocks;
    let mut font = None;
    let mut received = 0;
    let mut sequence = Vec::new();
    'query: while Instant::now() < deadline && received < 4096 {
        let mut descriptor = libc::pollfd {
            fd: input.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        let millis = deadline
            .saturating_duration_since(Instant::now())
            .as_millis()
            .max(1)
            .min(i32::MAX as u128) as i32;
        // No other code reads stdin until this function has returned.
        let ready = unsafe { libc::poll(&raw mut descriptor, 1, millis) };
        if ready < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            replay(&sequence, pending);
            return Err(error);
        }
        if ready == 0 || descriptor.revents & libc::POLLIN == 0 {
            break;
        }
        // Read one byte at a time: bytes after a terminal response (including
        // keys in the same write) must remain available to crossterm.
        let mut byte = [0];
        // StdinLock::read buffers ahead; crossterm reads the file descriptor
        // directly and would never see those buffered keys. Use an unbuffered
        // read on the same descriptor instead.
        let count = unsafe { libc::read(input.as_raw_fd(), byte.as_mut_ptr().cast(), 1) };
        if count < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            replay(&sequence, pending);
            return Err(error);
        }
        if count == 0 {
            break;
        }
        received += 1;
        // Esc is also the prefix of terminal replies and Alt keys. A second
        // Esc starts a fresh sequence: retain the first as a standalone key.
        // Without a following byte, a bare Esc remains ambiguous until timeout.
        if sequence == b"\x1b" && byte[0] == b'\x1b' {
            replay(&sequence, pending);
            sequence.clear();
            parser = Parser::new();
        }
        sequence.push(byte[0]);
        if response_prefix(&sequence) {
            let responses = parser.push(char::from(byte[0]));
            let complete = sequence.ends_with(b"\x1b\\")
                || sequence.starts_with(b"\x1b[")
                    && sequence
                        .last()
                        .is_some_and(|b| matches!(b, b'c' | b't' | b'n'));
            for response in responses {
                match response {
                    Response::Kitty => protocol = ProtocolType::Kitty,
                    Response::Sixel if protocol != ProtocolType::Kitty => {
                        protocol = ProtocolType::Sixel
                    }
                    Response::CellSize(Some((width, height)))
                        if (1..=128).contains(&width) && (1..=256).contains(&height) =>
                    {
                        font = Some((width, height))
                    }
                    Response::Status => {
                        sequence.clear();
                        break 'query;
                    }
                    _ => {}
                }
            }
            if complete {
                sequence.clear();
            }
        } else {
            // Keep collecting replies after early keys: a delayed Kitty reply
            // would otherwise become ordinary characters in crossterm. Replay
            // user input in order once DSR arrives or the deadline expires.
            if !key_complete(&sequence) {
                continue;
            }
            replay(&sequence, pending);
            sequence.clear();
        }
    }
    if !sequence.is_empty() {
        replay(&sequence, pending);
    }

    let font = font.or_else(|| {
        let mut size = std::mem::MaybeUninit::<libc::winsize>::zeroed();
        if unsafe { libc::ioctl(output.as_raw_fd(), libc::TIOCGWINSZ, size.as_mut_ptr()) } != 0 {
            return None;
        }
        let size = unsafe { size.assume_init() };
        if size.ws_col == 0 || size.ws_row == 0 {
            return None;
        }
        let (width, height) = (size.ws_xpixel / size.ws_col, size.ws_ypixel / size.ws_row);
        ((1..=128).contains(&width) && (1..=256).contains(&height)).then_some((width, height))
    });
    let Some(font) = font else {
        return Ok(Picker::halfblocks());
    };
    // The constructor is needed to supply a font size without invoking the
    // library's detached stdio query. All detection and bounds are ours.
    #[allow(deprecated)]
    let mut picker = Picker::from_fontsize(font);
    picker.set_protocol_type(if iterm {
        ProtocolType::Iterm2
    } else {
        protocol
    });
    Ok(picker)
}

fn iterm2(program: &str, lc_terminal: &str) -> bool {
    [
        "iTerm",
        "WezTerm",
        "mintty",
        "vscode",
        "Tabby",
        "Hyper",
        "rio",
        "Bobcat",
        "WarpTerminal",
    ]
    .iter()
    .any(|name| program.contains(name))
        || lc_terminal.contains("iTerm")
}

fn response_prefix(bytes: &[u8]) -> bool {
    // Frame every APC through its terminator, including Kitty replies with an
    // unexpected ID. Only replies matching our query enable graphics; unknown
    // reports are ignored whole and may leave detection using half blocks.
    // A key interleaved inside a report cannot be distinguished from its payload.
    if b"\x1b_".starts_with(bytes) || bytes.starts_with(b"\x1b_") {
        return true;
    }
    for prefix in [b"\x1b[?".as_slice(), b"\x1b[6;", b"\x1b[0n"] {
        if prefix.starts_with(bytes) {
            return true;
        }
        if let Some(tail) = bytes.strip_prefix(prefix) {
            return tail.iter().enumerate().all(|(i, b)| {
                b.is_ascii_digit()
                    || *b == b';'
                    || (i + 1 == tail.len() && matches!(b, b'c' | b't'))
            });
        }
    }
    false
}

fn key_complete(bytes: &[u8]) -> bool {
    if bytes.starts_with(b"\x1b[") || bytes.starts_with(b"\x1bO") {
        return bytes.len() >= 64
            || bytes.len() >= 3 && (0x40..=0x7e).contains(bytes.last().unwrap());
    }
    let text = bytes.strip_prefix(b"\x1b").unwrap_or(bytes);
    !text.is_empty() && std::str::from_utf8(text).is_ok()
}

fn escape_key(bytes: &[u8]) -> Option<KeyEvent> {
    let body = bytes
        .strip_prefix(b"\x1b[")
        .or_else(|| bytes.strip_prefix(b"\x1bO"))?;
    let (&last, parameters) = body.split_last()?;
    let parameters = std::str::from_utf8(parameters).ok()?;
    let code = match last {
        b'A' => KeyCode::Up,
        b'B' => KeyCode::Down,
        b'C' => KeyCode::Right,
        b'D' => KeyCode::Left,
        b'H' => KeyCode::Home,
        b'F' => KeyCode::End,
        b'Z' => KeyCode::BackTab,
        b'P'..=b'S' => KeyCode::F(last - b'P' + 1),
        b'~' => match parameters.split(';').next()? {
            "1" | "7" => KeyCode::Home,
            "2" => KeyCode::Insert,
            "3" => KeyCode::Delete,
            "4" | "8" => KeyCode::End,
            "5" => KeyCode::PageUp,
            "6" => KeyCode::PageDown,
            "11" => KeyCode::F(1),
            "12" => KeyCode::F(2),
            "13" => KeyCode::F(3),
            "14" => KeyCode::F(4),
            "15" => KeyCode::F(5),
            "17" => KeyCode::F(6),
            "18" => KeyCode::F(7),
            "19" => KeyCode::F(8),
            "20" => KeyCode::F(9),
            "21" => KeyCode::F(10),
            "23" => KeyCode::F(11),
            "24" => KeyCode::F(12),
            _ => return None,
        },
        _ => return None,
    };
    let modifier = parameters
        .split(';')
        .nth(1)
        .and_then(|value| value.parse::<u8>().ok())
        .unwrap_or(1)
        .saturating_sub(1);
    let mut modifiers = KeyModifiers::NONE;
    if modifier & 1 != 0 || code == KeyCode::BackTab {
        modifiers |= KeyModifiers::SHIFT;
    }
    if modifier & 2 != 0 {
        modifiers |= KeyModifiers::ALT;
    }
    if modifier & 4 != 0 {
        modifiers |= KeyModifiers::CONTROL;
    }
    Some(KeyEvent::new(code, modifiers))
}

fn replay(bytes: &[u8], pending: &mut VecDeque<Event>) {
    if let Some(key) = escape_key(bytes) {
        pending.push_back(Event::Key(key));
        return;
    }
    // Mouse, paste, focus and unrecognized terminal reports are whole control
    // sequences. Their payload must never become normal-mode shortcut keys.
    // During startup, paste wrappers are ignored and text is replayed as keys:
    // pasted newlines become Enter rather than part of an atomic Paste event.
    if bytes.starts_with(b"\x1b[") || bytes.starts_with(b"\x1bO") || bytes.starts_with(b"\x1b_") {
        return;
    }
    if let Some(tail) = bytes.strip_prefix(b"\x1b")
        && let Ok(text) = std::str::from_utf8(tail)
        && text.chars().count() == 1
    {
        pending.push_back(Event::Key(KeyEvent::new(
            KeyCode::Char(text.chars().next().unwrap()),
            KeyModifiers::ALT,
        )));
        return;
    }
    let text = String::from_utf8_lossy(bytes);
    for ch in text.chars() {
        let (code, modifiers) = match ch {
            '\x1b' => (KeyCode::Esc, KeyModifiers::NONE),
            '\r' | '\n' => (KeyCode::Enter, KeyModifiers::NONE),
            '\t' => (KeyCode::Tab, KeyModifiers::NONE),
            '\x08' | '\x7f' => (KeyCode::Backspace, KeyModifiers::NONE),
            '\x01'..='\x1a' => (
                KeyCode::Char(char::from(ch as u8 + b'a' - 1)),
                KeyModifiers::CONTROL,
            ),
            ch => (KeyCode::Char(ch), KeyModifiers::NONE),
        };
        pending.push_back(Event::Key(KeyEvent::new(code, modifiers)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{io::Read, os::unix::net::UnixStream};

    #[test]
    fn tmux_requires_server_support_and_an_exact_client_sixel_feature() {
        for features in ["sixel", "RGB,sixel", "sixel,RGB", "RGB,sixel,clipboard"] {
            assert_eq!(
                tmux_font(&format!("/dev/pts/7\t12\t23\t1\t{features}\n")),
                Some((12, 23))
            );
        }
        for report in [
            "",
            "\t12\t23\t1\tsixel\n",
            "/dev/pts/7\t12\t23\t0\tsixel\n",
            "/dev/pts/7\t12\t23\t\tsixel\n",
            "/dev/pts/7\t12\t23\t1\t\n",
            "/dev/pts/7\t12\t23\t1\tRGB,clipboard\n",
            "/dev/pts/7\t12\t23\t1\tnosixel,sixel-extra\n",
            "/dev/pts/7\t0\t23\t1\tsixel\n",
            "/dev/pts/7\t12\t0\t1\tsixel\n",
            "/dev/pts/7\t-1\t23\t1\tsixel\n",
            "/dev/pts/7\t129\t23\t1\tsixel\n",
            "/dev/pts/7\t12\t257\t1\tsixel\n",
            "/dev/pts/7\tunknown\t23\t1\tsixel\n",
        ] {
            assert_eq!(tmux_font(report), None, "{report:?}");
        }
    }

    #[test]
    fn tmux_encodes_raw_sixel_with_client_geometry_and_halfblock_fallback() {
        use ratatui::{buffer::Buffer, widgets::Widget};
        let size = Rect::new(0, 0, 12, 6);
        for font in [(12, 23), (20, 40)] {
            let protocol = ImagePicker::Tmux(Some(font))
                .new_protocol(image::DynamicImage::new_rgb8(96, 48), size)
                .unwrap();
            let Protocol::Sixel(sixel) = &protocol else {
                panic!("expected Sixel");
            };
            assert!(!sixel.is_tmux);
            assert!(sixel.data.starts_with("\x1bP"));
            assert!(sixel.data.ends_with("\x1b\\"));
            assert!(!sixel.data.contains("tmux;"));
            assert_eq!(sixel.area.width, 12);
            assert_eq!(sixel.area.height, if font.0 == 12 { 4 } else { 3 });
            let mut buffer = Buffer::empty(size);
            ratatui_image::Image::new(&protocol).render(size, &mut buffer);
            assert_eq!(buffer[(0, 0)].symbol(), sixel.data);
        }
        let protocol = ImagePicker::Tmux(None)
            .new_protocol(image::DynamicImage::new_rgb8(96, 96), size)
            .unwrap();
        assert!(matches!(protocol, Protocol::Halfblocks(_)));
        assert_eq!(protocol.area(), size);
    }

    #[test]
    fn silent_terminal_times_out_without_retaining_an_input_reader() {
        let (mut input, mut terminal) = UnixStream::pair().unwrap();
        let started = Instant::now();
        let picker = query(
            &mut input,
            &mut std::fs::OpenOptions::new()
                .write(true)
                .open("/dev/null")
                .unwrap(),
            Duration::from_millis(20),
            false,
            &mut VecDeque::new(),
        )
        .unwrap();
        assert_eq!(picker.protocol_type(), ProtocolType::Halfblocks);
        assert!(started.elapsed() < Duration::from_secs(1));
        terminal.write_all(b"q").unwrap();
        input
            .set_read_timeout(Some(Duration::from_secs(1)))
            .unwrap();
        let mut key = [0];
        input.read_exact(&mut key).unwrap();
        assert_eq!(&key, b"q");
    }

    #[test]
    fn early_keys_and_bytes_after_status_are_preserved() {
        for (prefix, keys, consumed) in [
            ("", ":sync\r", true),
            ("\x1b[6;16;8t", "台标\r", true),
            ("\x1b[0n", ":sync\r", false),
        ] {
            let (mut input, mut terminal) = UnixStream::pair().unwrap();
            terminal
                .write_all(format!("{prefix}{keys}").as_bytes())
                .unwrap();
            let mut pending = VecDeque::new();
            query(
                &mut input,
                &mut std::fs::OpenOptions::new()
                    .write(true)
                    .open("/dev/null")
                    .unwrap(),
                Duration::from_millis(50),
                false,
                &mut pending,
            )
            .unwrap();
            let consumed = if consumed {
                for ch in keys.trim_end_matches('\r').chars() {
                    assert_eq!(
                        pending.pop_front(),
                        Some(Event::Key(KeyEvent::new(
                            KeyCode::Char(ch),
                            KeyModifiers::NONE
                        )))
                    );
                }
                assert_eq!(
                    pending.pop_front(),
                    Some(Event::Key(KeyEvent::new(
                        KeyCode::Enter,
                        KeyModifiers::NONE
                    )))
                );
                keys.len()
            } else {
                0
            };

            assert!(pending.is_empty());
            let mut remaining = vec![0; keys.len() - consumed];
            input.read_exact(&mut remaining).unwrap();
            assert_eq!(remaining, keys.as_bytes()[consumed..]);
        }
    }

    #[test]
    fn unknown_control_sequences_do_not_become_shortcuts() {
        let (mut input, mut terminal) = UnixStream::pair().unwrap();
        terminal
            .write_all(b"\x1b[<0;20;7M\x1b[200~\x1b[201~\x1b[I\x1b[O\x1bOXq\x1b[0n")
            .unwrap();
        let mut pending = VecDeque::new();
        query(
            &mut input,
            &mut std::fs::OpenOptions::new()
                .write(true)
                .open("/dev/null")
                .unwrap(),
            Duration::from_millis(50),
            false,
            &mut pending,
        )
        .unwrap();
        assert_eq!(
            pending,
            [Event::Key(KeyEvent::new(
                KeyCode::Char('q'),
                KeyModifiers::NONE
            ))]
        );
        // An incomplete unknown sequence is ignored at the deadline too.
        pending.clear();
        replay(b"\x1b[<0;", &mut pending);
        replay(b"\x1b", &mut pending);
        assert_eq!(
            pending,
            [Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))]
        );
    }

    #[test]
    fn delayed_terminal_replies_after_keys_are_consumed_until_status() {
        let (mut input, mut terminal) = UnixStream::pair().unwrap();
        input
            .set_read_timeout(Some(Duration::from_secs(1)))
            .unwrap();
        terminal.write_all(b":s").unwrap();
        let replies = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(20));
            terminal
                .write_all(b"\x1b_Gi=31;OK\x1b\\ync\r\x1b[6;16;8t\x1b[0nq")
                .unwrap();
        });
        let mut pending = VecDeque::new();
        let picker = query(
            &mut input,
            &mut std::fs::OpenOptions::new()
                .write(true)
                .open("/dev/null")
                .unwrap(),
            Duration::from_millis(500),
            false,
            &mut pending,
        )
        .unwrap();
        replies.join().unwrap();
        assert_eq!(picker.protocol_type(), ProtocolType::Kitty);
        for code in [
            KeyCode::Char(':'),
            KeyCode::Char('s'),
            KeyCode::Char('y'),
            KeyCode::Char('n'),
            KeyCode::Char('c'),
            KeyCode::Enter,
        ] {
            assert_eq!(
                pending.pop_front(),
                Some(Event::Key(KeyEvent::new(code, KeyModifiers::NONE)))
            );
        }
        assert!(pending.is_empty());
        let mut key = [0];
        input.read_exact(&mut key).unwrap();
        assert_eq!(&key, b"q");
    }

    #[test]
    fn escape_before_a_terminal_reply_remains_a_separate_key() {
        for (reply, expected) in [
            ("\x1b_Gi=31;OK\x1b\\", ProtocolType::Kitty),
            ("\x1b[?1;4c", ProtocolType::Sixel),
        ] {
            let (mut input, mut terminal) = UnixStream::pair().unwrap();
            terminal
                .write_all(format!("\x1b{reply}\x1b[6;16;8t\x1b[0n").as_bytes())
                .unwrap();
            let mut pending = VecDeque::new();
            let picker = query(
                &mut input,
                &mut std::fs::OpenOptions::new()
                    .write(true)
                    .open("/dev/null")
                    .unwrap(),
                Duration::from_millis(50),
                false,
                &mut pending,
            )
            .unwrap();
            assert_eq!(picker.protocol_type(), expected);
            assert_eq!(
                pending,
                [Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))]
            );
        }
    }

    #[test]
    fn unrecognized_graphics_replies_do_not_become_keys() {
        for reply in ["\x1b_Gi=32;OK\x1b\\", "\x1b_Gi=31;ERROR\x1b\\"] {
            let (mut input, mut terminal) = UnixStream::pair().unwrap();
            terminal
                .write_all(format!("{reply}q\x1b[6;16;8t\x1b[0n").as_bytes())
                .unwrap();
            let mut pending = VecDeque::new();
            let picker = query(
                &mut input,
                &mut std::fs::OpenOptions::new()
                    .write(true)
                    .open("/dev/null")
                    .unwrap(),
                Duration::from_millis(50),
                false,
                &mut pending,
            )
            .unwrap();
            assert_eq!(picker.protocol_type(), ProtocolType::Halfblocks);
            assert_eq!(
                pending,
                [Event::Key(KeyEvent::new(
                    KeyCode::Char('q'),
                    KeyModifiers::NONE
                ))]
            );
        }
    }

    #[test]
    fn navigation_keys_are_replayed_as_single_events() {
        for (sequence, code, modifiers) in [
            ("\x1b[A", KeyCode::Up, KeyModifiers::NONE),
            ("\x1b[6~", KeyCode::PageDown, KeyModifiers::NONE),
            ("\x1b[1;5C", KeyCode::Right, KeyModifiers::CONTROL),
            ("\x1bOP", KeyCode::F(1), KeyModifiers::NONE),
            ("\x1b台", KeyCode::Char('台'), KeyModifiers::ALT),
        ] {
            let (mut input, mut terminal) = UnixStream::pair().unwrap();
            terminal.write_all(sequence.as_bytes()).unwrap();
            let mut pending = VecDeque::new();
            query(
                &mut input,
                &mut std::fs::OpenOptions::new()
                    .write(true)
                    .open("/dev/null")
                    .unwrap(),
                Duration::from_millis(50),
                false,
                &mut pending,
            )
            .unwrap();
            assert_eq!(
                pending.pop_front(),
                Some(Event::Key(KeyEvent::new(code, modifiers)))
            );
            assert!(pending.is_empty());
        }
    }

    #[test]
    fn capability_responses_select_protocols_with_bounded_font_sizes() {
        for (response, iterm, expected) in [
            (
                "\x1b_Gi=31;OK\x1b\\\x1b[6;16;8t\x1b[0n",
                false,
                ProtocolType::Kitty,
            ),
            ("\x1b[?1;4c\x1b[6;16;8t\x1b[0n", false, ProtocolType::Sixel),
            ("\x1b[6;16;8t\x1b[0n", true, ProtocolType::Iterm2),
            (
                "\x1b_Gi=31;OK\x1b\\\x1b[6;0;0t\x1b[0n",
                false,
                ProtocolType::Halfblocks,
            ),
        ] {
            let (mut input, mut terminal) = UnixStream::pair().unwrap();
            terminal.write_all(response.as_bytes()).unwrap();
            let picker = query(
                &mut input,
                &mut std::fs::OpenOptions::new()
                    .write(true)
                    .open("/dev/null")
                    .unwrap(),
                Duration::from_millis(50),
                iterm,
                &mut VecDeque::new(),
            )
            .unwrap();
            assert_eq!(picker.protocol_type(), expected);
        }
    }
}
