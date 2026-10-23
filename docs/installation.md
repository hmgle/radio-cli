# Installation guide

This guide covers runtime requirements and every supported installation method.
For project orientation and daily usage, start with the
[README](../README.md).

## Requirements

- Linux or macOS. Linux is the primary target.
- `mpv`.
- `ffmpeg` and `ffprobe`.
- Rust 1.88 or newer only when building from source.

SQLite is bundled into the executable, so no SQLite command-line package is
needed. Install the media tools with your package manager, for example:

```sh
# Debian / Ubuntu
sudo apt install mpv ffmpeg

# macOS
brew install mpv ffmpeg
```

## Install from npm

```sh
npm install -g @hmgle/radio-cli
```

This installs the prebuilt `radio` binary for your platform (Linux x86_64, macOS
Intel, or Apple Silicon) and needs Node.js 18 or newer. `mpv`, `ffmpeg`, and
`ffprobe` are still required at runtime.

## Install a prebuilt release

Release archives, English shell completions, a manual page, and a combined
`SHA256SUMS` are published on the
[releases page](https://github.com/hmgle/radio-cli/releases). They do not require
a Rust toolchain.

| Platform | Release target | Status |
| --- | --- | --- |
| Linux x86_64 | `x86_64-unknown-linux-musl` | Primary; statically linked |
| macOS Apple Silicon | `aarch64-apple-darwin` | Experimental |
| macOS Intel | `x86_64-apple-darwin` | Experimental |

Download `SHA256SUMS` and the archive for your platform, then verify, extract,
and run it from the download directory. Substitute your platform's target from
the table above for `x86_64-unknown-linux-musl`; the `*` in archive names
matches the release version, so the same commands work for every release:

```sh
# Verify the download against the published checksums.
grep -F 'x86_64-unknown-linux-musl' SHA256SUMS | sha256sum -c -
# On macOS: grep -F 'aarch64-apple-darwin' SHA256SUMS | shasum -a 256 -c -

# Extract the archive and start radio.
tar -xzf radio-cli-v*-x86_64-unknown-linux-musl.tar.gz
cd radio-cli-v*-x86_64-unknown-linux-musl
./radio doctor
./radio sync
./radio tui
```

The macOS packages are not Developer ID-signed or notarized. If Gatekeeper
blocks a download you trust, verify its checksum first, then remove quarantine
from the extracted binary with `xattr -d com.apple.quarantine ./radio`. See
[Apple's guidance](https://support.apple.com/en-us/102445).

## Install from crates.io

```sh
cargo install radio-cli --locked
```

`cargo install` builds from source, so it needs a Rust 1.88 or newer toolchain
and installs the `radio` binary into `~/.cargo/bin`. `mpv`, `ffmpeg`, and
`ffprobe` are still required at runtime.

## Build from source

```sh
git clone https://github.com/hmgle/radio-cli.git
cd radio-cli
cargo build --release --locked
./target/release/radio doctor
./target/release/radio sync
./target/release/radio tui

# Or install the executable into ~/.cargo/bin.
cargo install --path . --locked
```

## First run

`radio` opens the TUI when both input and output are interactive terminals; if
either stream is redirected, it prints help instead. `radio tui` always requests
the TUI.

If no catalog exists, the TUI and `radio random` first fetch the 1,000 most
popular stations. Run `radio sync` to cache the full catalog. A failed sync
keeps the previous catalog intact. `radio doctor` checks mpv, FFmpeg, ffprobe,
the recording directory, and free space.
