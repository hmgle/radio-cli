# Contributing

Thanks for your interest in radio-cli. Bug reports, feature requests, and pull
requests are welcome.

## Ground rules

- radio-cli is **Linux-first**, with basic CLI/TUI support on macOS.
  Keep shared Unix behavior portable and isolate platform-specific operations.
  Native Windows support is outside the current scope.
- The README is the user-facing documentation; design details live in `docs/`
  (currently written in Chinese).
- Keep pull requests focused. One logical change per PR is easiest to review.

## Local development

See the README for prerequisites (Rust 1.88+, mpv, FFmpeg). Typical loop:

```sh
cargo fmt --check
cargo clippy --all-targets --all-features --locked -- -D warnings
cargo test --all-targets --all-features --locked
```

Formatting is checked on Linux; Clippy and tests run on Linux and macOS.
All CI gates must pass before merge.

## Tests

- Tests use local HTTP/ICY/HLS sources and never depend on public radio
  stations or the public network.
- Recording tests require FFmpeg. mpv IPC tests use a null audio output and are
  skipped when mpv is unavailable; point them at a binary with
  `RADIO_TEST_MPV=/absolute/path/to/mpv` to run them locally.
- On macOS, the Vorbis fixture needs the encoder in Homebrew's `ffmpeg-full`:
  run `brew install ffmpeg-full mpv` and
  `export PATH="$(brew --prefix ffmpeg-full)/bin:$PATH"` before testing.
- Use `--data-dir <dir>` (and `--config <file>` when needed) so tests and manual
  runs never touch the default data directory.

## Commits

Short, imperative subject lines in the style of the existing history, e.g.
`Add station import command`. Explain the "why" in the body when it isn't
obvious.

## Reporting bugs

Use the issue templates and include your OS, radio-cli version, and mpv/FFmpeg
versions. Security issues go through the process in [SECURITY.md](SECURITY.md).
