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

Formatting is checked on Linux; Clippy and tests run on Linux and both Apple
Silicon and Intel macOS runners.
All CI gates must pass before merge.

## Release archives

Run `scripts/package-release.sh [target]` from a clean tracked checkout on the
target's native host. Supported targets are `x86_64-unknown-linux-musl`,
`aarch64-apple-darwin`, and `x86_64-apple-darwin`. Linux needs `musl-tools`;
macOS needs `brew install gnu-tar`. Install the Rust target with `rustup target
add <target>` and ensure mpv, FFmpeg and ffprobe are on `PATH`.

The script checks the executable architecture, creates the archive, then runs
`--version` and `doctor` from the extracted package with isolated configuration.
Each archive includes `BUILDINFO`, English completions and the manual. CI only
assembles the `release-archives` artifact and combined `SHA256SUMS` after all
test, MSRV, packaging and audit jobs pass; publishing a GitHub Release is a
separate step.

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

Run the local catalog measurements with:

```sh
cargo test --release --test storage catalog_benchmark -- --ignored --nocapture
cargo test --release --test catalog_perf -- --ignored --nocapture
```

The second command builds a deterministic 200,000-station fixture and compares
legacy SQL against current queries, reporting P50/P95 for each search and
sorting case, counts, station location, facets, URL checks, and migration cost.
It also reports index sizes. Run on an otherwise idle machine; its five-sample
P95 is the slowest observed sample and is sensitive to other disk and CPU work.

## Commits

Short, imperative subject lines in the style of the existing history, e.g.
`Add station import command`. Explain the "why" in the body when it isn't
obvious.

## Reporting bugs

Use the issue templates and include your OS, radio-cli version, and mpv/FFmpeg
versions. Security issues go through the process in [SECURITY.md](SECURITY.md).
