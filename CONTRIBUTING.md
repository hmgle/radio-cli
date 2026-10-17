# Contributing

Thanks for your interest in radio-cli. Bug reports, feature requests, and pull
requests are welcome.

## Ground rules

- radio-cli is **Linux-first**. The code uses Unix APIs unconditionally, so
  Windows/macOS support is not a goal for now; please don't add
  platform-specific shims without discussing it first.
- The README is the user-facing documentation; design details live in `docs/`
  (currently written in Chinese).
- Keep pull requests focused. One logical change per PR is easiest to review.

## Local development

See the README for prerequisites (Rust 1.88+, mpv, FFmpeg). Typical loop:

```sh
cargo fmt --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --all-targets --locked
```

All three gates run in CI and must pass before merge.

## Tests

- Tests use local HTTP/ICY/HLS sources and never depend on public radio
  stations or the public network.
- Recording tests require FFmpeg. mpv IPC tests use a null audio output and are
  skipped when mpv is unavailable; point them at a binary with
  `RADIO_TEST_MPV=/absolute/path/to/mpv` to run them locally.
- Use `--data-dir <dir>` (and `--config <file>` when needed) so tests and manual
  runs never touch the default data directory.

## Commits

Short, imperative subject lines in the style of the existing history, e.g.
`Add station import command`. Explain the "why" in the body when it isn't
obvious.

## Reporting bugs

Use the issue templates and include your OS, radio-cli version, and mpv/FFmpeg
versions. Security issues go through the process in [SECURITY.md](SECURITY.md).
