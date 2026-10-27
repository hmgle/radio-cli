# Contributing

Thanks for your interest in radio-cli. Bug reports, feature requests, and pull
requests are welcome.

## Ground rules

- radio-cli is **Linux-first**, with basic CLI/TUI support on macOS.
  Keep shared Unix behavior portable and isolate platform-specific operations.
  Native Windows support is outside the current scope.
- `README.md` and `README.zh-CN.md` are the user-facing entry points. Public
  user guides are the paired files under `docs/`; `docs/tui.md` is the internal
  TUI design reference, and other design and validation notes are internal
  unless explicitly designated public.
- Keep English and Chinese public docs structurally synchronized in the same
  pull request. Do not hardcode release versions in install commands; use a
  wildcard for the version, derive archive names from `SHA256SUMS`, or link to
  the releases page.
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

## Publishing a release

Tag releases as `vX.Y.Z`; the tag has to match the `version` in `Cargo.toml`.
For each release: move the `Unreleased` notes in `CHANGELOG.md` under the new
version, bump `version`, commit, push the tag, and create the GitHub Release
with the `release-archives` artifact. Publishing that release runs the
`Publish to crates.io` workflow, which verifies the tag and runs
`cargo publish --locked`.

crates.io publishing uses trusted publishing, so the workflow needs no API token
secret. The first version of a crate cannot use it: create a crates.io API token
with the `publish-new` and `publish-update` scopes, run `cargo login`, and
publish that version by hand. Afterwards configure the crate on crates.io under
Settings → Trusted Publishing with repository owner `hmgle`, repository name
`radio-cli`, workflow filename `publish-crates-io.yml`, and environment
`release`. The workflow creates the `release` environment on its first run if it
does not exist; create it beforehand only to add protection rules such as
required reviewers.

Uploads to crates.io are permanent: a published version cannot be replaced or
deleted, so confirm the tag and version before publishing. The workflow can also
be started manually with `workflow_dispatch`: select the existing `vX.Y.Z` tag
as the workflow ref. Both publishing workflows reject branch refs and tags
that do not exactly match the version in `Cargo.toml`.

If a rerun reports that the crate version already exists, treat that as evidence
the previous upload succeeded; do not bump the version to recover.

The npm distribution is four packages that share the crate version:
`@hmgle/radio-cli` holds the launcher, and `@hmgle/radio-cli-linux-x64`,
`@hmgle/radio-cli-darwin-x64`, and `@hmgle/radio-cli-darwin-arm64` hold the
prebuilt binaries as optional dependencies. `scripts/package-npm.sh <target>`
stamps the versions from `Cargo.toml` and assembles `npm/dist/` from the binary
that `scripts/package-release.sh` produced; `scripts/package-npm.sh --wrapper`
assembles only the launcher package.

Publishing the release also runs `.github/workflows/publish-npm.yml`. Each
platform job builds and smoke-tests its binary, packs the npm packages, verifies
an isolated install with `radio --version` and `radio doctor`, and publishes the
platform package with `npm publish --provenance`; the wrapper package is
published last, after every platform package succeeded. npm trusted publishing
needs npm 11.5.1 or newer (the workflow pins Node 24) and one trusted publisher
per package on npmjs.com. A package has to exist before a trusted publisher can
be configured, so a brand-new package needs one manual publish; these four
packages are already configured. Each publisher uses organization or user
`hmgle`, repository `radio-cli`, workflow filename `publish-npm.yml`,
environment `release`, and `npm publish` allowed. A newly created publisher
must complete its first successful verified publish within two days. npm may
require interactive WebAuthn for publisher changes; configure them through the
package settings page or `npm trust github <package> --file publish-npm.yml
--repo hmgle/radio-cli --env release --allow-publish`. After a partial failure,
use GitHub's **Re-run failed jobs** to retry only unfinished jobs. Re-running all
jobs or manually dispatching the same version tag also works: the publish
script checks the exact package version and skips an existing upload. Registry
lookup failures other than a missing version, and publish failures, still fail
the job. Trusted publishing uses OIDC without a `NODE_AUTH_TOKEN` secret.

## Tests

- Tests use local HTTP/ICY/HLS sources and never depend on public radio
  stations or the public network.
- Recording tests require FFmpeg. mpv IPC tests use a null audio output and are
  skipped when mpv is unavailable; point them at a binary with
  `RADIO_TEST_MPV=/absolute/path/to/mpv` to run them locally.
- On macOS, the Vorbis fixture needs the encoder in Homebrew's `ffmpeg-full`:
  run `brew install ffmpeg-full mpv` and
  `export PATH="$(brew --prefix ffmpeg-full)/bin:$PATH"` before testing.
- Run `uv run --no-project --python 3.12 python tests/speech_worker.py` for
  deterministic speech-buffer tests; no models or speech packages are needed.
- The ignored `real_models_transcribe_a_local_http_fixture` test requires
  `RADIO_TEST_SPEECH_DATA` pointing at an isolated data directory with both
  models installed, and `RADIO_TEST_SPEECH_WAV` pointing at local speech audio.
  It makes no public station requests or model downloads. Run it with
  `cargo test --lib real_models_transcribe_a_local_http_fixture -- --ignored --nocapture`.
- Use `--data-dir <dir>` (and `--config <file>` when needed) so tests and manual
  runs never touch the default data directory.

Run the local catalog measurements with:

```sh
cargo test --release --test storage catalog_benchmark -- --ignored --nocapture
cargo test --release --test catalog_perf -- --ignored --nocapture
# Linux only: resource sampling uses /proc.
uv run scripts/measure_resources.py --binary target/release/radio
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
