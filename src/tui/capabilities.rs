//! Query once before event polling. Older image pickers leave a detached stdin
//! reader behind on timeout, so own the deadline and the only read here.
use ratatui_image::picker::{
    Picker, ProtocolType,
    cap_parser::{Parser, QueryStdioOptions, Response},
};
use std::{
    io::{self, Read, Write},
    os::fd::AsRawFd,
    time::{Duration, Instant},
};

pub(super) fn picker() -> Picker {
    if std::env::var_os("TMUX").is_some() {
        return Picker::halfblocks();
    }
    let program = std::env::var("TERM_PROGRAM").unwrap_or_default();
    let iterm = matches!(program.as_str(), "iTerm.app" | "WezTerm" | "rio" | "Bobcat");
    query(
        &mut io::stdin().lock(),
        &mut io::stdout().lock(),
        Duration::from_millis(200),
        iterm,
    )
    .unwrap_or_else(|_| Picker::halfblocks())
}

fn query(
    input: &mut (impl Read + AsRawFd),
    output: &mut impl Write,
    timeout: Duration,
    iterm: bool,
) -> io::Result<Picker> {
    output.write_all(Parser::query(false, QueryStdioOptions::default()).as_bytes())?;
    output.flush()?;
    let deadline = Instant::now() + timeout;
    let mut parser = Parser::new();
    let mut protocol = ProtocolType::Halfblocks;
    let mut font = None;
    let mut received = 0;
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
            return Err(error);
        }
        if ready == 0 || descriptor.revents & libc::POLLIN == 0 {
            break;
        }
        let mut bytes = [0; 256];
        let count = input.read(&mut bytes)?;
        if count == 0 {
            break;
        }
        received += count;
        for byte in &bytes[..count] {
            for response in parser.push(char::from(*byte)) {
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
                    Response::Status => break 'query,
                    _ => {}
                }
            }
        }
    }
    let font = font.or_else(|| {
        let mut size = std::mem::MaybeUninit::<libc::winsize>::zeroed();
        if unsafe { libc::ioctl(input.as_raw_fd(), libc::TIOCGWINSZ, size.as_mut_ptr()) } != 0 {
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixStream;

    #[test]
    fn silent_terminal_times_out_without_retaining_an_input_reader() {
        let (mut input, mut terminal) = UnixStream::pair().unwrap();
        let started = Instant::now();
        let picker = query(
            &mut input,
            &mut Vec::new(),
            Duration::from_millis(20),
            false,
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
                &mut Vec::new(),
                Duration::from_millis(50),
                iterm,
            )
            .unwrap();
            assert_eq!(picker.protocol_type(), expected);
        }
    }
}
