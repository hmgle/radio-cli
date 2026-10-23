# @hmgle/radio-cli

Terminal radio for Linux and macOS: browse the Radio Browser catalog, search and
filter locally, play any HTTP(S) stream, keep favorites and history, and record
streams as stream copies.

## Install

```sh
npm install -g @hmgle/radio-cli
```

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
