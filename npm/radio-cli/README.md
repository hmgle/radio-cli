# @hmgle/radio-cli

Terminal radio for Linux and macOS: browse the Radio Browser catalog, search and
filter locally, play any HTTP(S) stream, keep favorites and history, and record
streams as stream copies.

## Install

```sh
npm install -g @hmgle/radio-cli
```

Requires Node.js 18 or newer. Prebuilt binaries are available for Linux x64,
macOS Intel x64, and macOS Apple Silicon arm64.

This installs the prebuilt `radio` binary for your platform. Playback and
recording still need `mpv`, `ffmpeg`, and `ffprobe` installed separately:

```sh
# Debian / Ubuntu
sudo apt install mpv ffmpeg

# macOS
brew install mpv ffmpeg
```

Then run `radio`:

```sh
radio --version
radio sync
radio tui
```

Windows is not supported.

## Documentation

See the [radio-cli repository](https://github.com/hmgle/radio-cli) for the full
README, the configuration reference, and the troubleshooting guide.
