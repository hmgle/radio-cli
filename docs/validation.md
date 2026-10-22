# 验证记录

## 0.8.0 release candidate — 2026-10-22

- Linux all-target/all-feature tests: 345 passed, zero failures (lib 245, cli 29,
  media 34, storage 24, tui 7, directory 6); storage 与 catalog_perf 的 3 个
  ignored 基准不计入。格式化与严格 all-target/all-feature Clippy 在锁定依赖下通过。
  注意：`tests/cli.rs` 的辅助函数只隔离 `--data-dir`，不断开默认配置路径，因此本机
  `~/.config/radio-cli/config.toml` 设 `language = "zh-CN"` 时，两条断言英文文案的
  CLI 用例会失败。本次运行将 `XDG_CONFIG_HOME` 指向空目录（与 CI 无用户配置的条件
  一致）后全部通过。
- The explicit release benchmark passes with 50,000 synthetic stations and
  search P95 30.18 ms, below the 100 ms budget. This is one local observation,
  not a comparative performance claim.
- `bash -n scripts/package-release.sh` passes and `radio --version` reports
  `radio 0.8.0`. Linux x86_64 musl packaging passes architecture and
  static-linkage checks plus version and doctor smoke checks from the extracted
  archive; the archive carries nonempty guides, English completions, the manual
  and `BUILDINFO`, and `SHA256SUMS` covers the produced tarball.
- Release GitHub CI gates all pass for this candidate (run 39814611849 on
  `main`): Linux tests, Rust 1.88 MSRV, RustSec audit, x86_64 musl packaging,
  native macOS Apple Silicon and Intel clippy/tests/packaging, and the verified
  release-archives job. The Intel macOS job first failed on two real-mpv tests
  (`live_stream_reconnects_across_outages_without_duplicate_history`,
  `engine_uses_explicit_backup_without_merging_names`) with 10-second startup
  timeouts; the same tests pass on Linux, on Apple Silicon and locally, and the
  re-run of that job passed, so it is runner timing flakiness rather than a
  regression. Published archives come from the CI artifact and were verified
  against `SHA256SUMS`. Native package checks do not imply Developer ID
  signing, notarization or physical listening.

验证日期：2026-10-22。环境：Debian Linux x86_64，内核 6.12.113+deb13-amd64，
Intel Core i5-8365U（8 个逻辑 CPU），Rust 1.97.1、FFmpeg 7.1.5、mpv 0.40.0。

## 0.7.0 release candidate — 2026-10-21

- Linux all-target/all-feature tests: 234 passed, zero failures; the separate
  catalog benchmark and the 200,000-station performance matrix are excluded from
  that count. Formatting and strict all-target/all-feature Clippy pass with the
  locked dependencies.
- The explicit release benchmark passes with 50,000 synthetic stations and
  search P95 26.92 ms, below the 100 ms budget. This is one local observation,
  not a comparative performance claim.
- Catalog reads move to a persistent reader connection on a dedicated thread, so
  slow searches no longer delay media controls. Requests coalesce, stale
  generations are discarded, exact totals are cached across pages, and queries
  are interrupted during shutdown. A regression test drives a blocked reader
  while a volume command still completes.
- Substring search uses FTS5 trigram candidates chosen from cached per-trigram
  document frequencies, with the exact literal re-checked by `instr`. Queries
  shorter than three characters, queries whose rarest sampled trigram is too
  common, and special characters fall back to scan paths, so Chinese territory
  matching and existing sorting and pagination behavior are unchanged. A
  differential check against brute-force `instr` over 720 filter combinations
  found no row-set, ordering or total differences.
- Distinct facets are cached during publication and maintained incrementally,
  removing catalog-wide token splitting from interactive queries. Normalized
  stream URLs are indexed, so duplicate detection no longer decodes every
  station JSON blob. Station location counts preceding indexed sort ranges
  instead of ranking the whole filtered set.
- Search-planning statistics are collected after catalog publication in a read
  snapshot, then replaced in a short transaction, so advisory maintenance never
  holds the main write lock and cannot fail an already published catalog.
- Existing databases migrate atomically to schema v5 on first open, building
  search indexes and facet caches without requiring a catalog sync. The
  deterministic 200,000-station fixture occupies about 243 MiB with schema v5
  (about 144 MiB for the equivalent v2 fixture) and its first-open v2-to-v5
  migration took about 5.3 seconds locally. Older binaries cannot open the
  upgraded database.
- `bash -n scripts/package-release.sh` passes and `radio --version` reports
  `radio 0.7.0`. Linux x86_64 musl packaging passes architecture and
  static-linkage checks plus version and doctor smoke checks from the extracted
  archive; the archive carries nonempty guides, English completions, the manual
  and `BUILDINFO`, and `SHA256SUMS` covers the produced tarball.
- Release archives still require the final GitHub CI gates for Linux, native
  macOS Apple Silicon and Intel, Rust 1.88 and RustSec auditing, plus the musl
  packaging and extracted-archive smoke checks. Those jobs were not run for this
  candidate. Native package checks do not imply Developer ID signing,
  notarization or physical listening.

验证日期：2026-10-21。环境：Debian Linux x86_64，内核 6.12.113+deb13-amd64，
Intel Core i5-8365U（8 个逻辑 CPU），Rust 1.97.1。

## 0.6.1 release candidate — 2026-10-20

- Linux all-target/all-feature tests: 215 passed, zero failures; the separate
  catalog benchmark is excluded from that count. Formatting and strict
  all-target/all-feature Clippy pass with the locked dependencies.
- The explicit release benchmark passes with 50,000 synthetic stations and
  search P95 28.64 ms, below the 100 ms budget. This is one local observation,
  not a comparative performance claim.
- Chinese territory localization covers display, filtering, search and JSON
  compatibility. Details, the filter popup and its candidates render CLDR names
  while storage, `--json`/`--json-v2` output and `--country` keep upstream codes;
  a cross-language CLI test asserts both catalogs stay byte-identical.
- Search resolves Chinese names to territory codes at query time, so existing
  catalogs need no resync or migration. Two-character queries match territory
  names as substrings; single-character queries match station text only, since
  one character is shared by too many territories (`岛` appears in 32 names).
  Missing and unrecognized codes fall back to the upstream catalog name, then
  the code, then unknown, so no catalog metadata is dropped from details.
- The catalog source notice follows the active interface language; the English
  translation is covered by a test that also asserts the Chinese text is absent
  from the English panel.
- Existing 80×24, 100×24 and 160×48 layout snapshots were reviewed and needed no
  change: every snapshot station uses a recognized code and keeps its CLDR name.
- `bash -n scripts/package-release.sh` passes and `radio --version` reports
  `radio 0.6.1`.
- Release archives still require the final GitHub CI gates for Linux, native
  macOS Apple Silicon and Intel, Rust 1.88 and RustSec auditing, plus the musl
  packaging and extracted-archive smoke checks. Those jobs were not run for this
  candidate. Native package checks do not imply Developer ID signing, notarization
  or physical listening.

验证日期：2026-10-20。环境：Debian Linux x86_64，内核 6.12.113+deb13-amd64，
Intel Core i5-8365U（8 个逻辑 CPU），Rust 1.97.1。

## 0.6.0 release candidate — 2026-10-20

- Linux all-target/all-feature tests: 209 passed, zero failures; the separate
  catalog benchmark is excluded from that count. Formatting, strict Clippy and
  Rust 1.88 checks pass with the locked dependencies.
- A native Apple Silicon macOS run passes 207 tests with zero failures, including
  pseudo-terminal cases; two Linux-only CLI cases are excluded. HTTP fixtures
  explicitly clear inherited nonblocking mode on accepted sockets so Darwin
  tests actually wait for cancellation and connection handoffs.
- The explicit release benchmark passes with 50,000 synthetic stations and
  search P95 33.57 ms, below the 100 ms budget. This is one local observation,
  not a comparative performance claim.
- Artwork coverage uses local HTTP/ICY fixtures for JPEG/PNG validation,
  download/decode limits, persistent cache reuse, URL changes, cancellation,
  track association and the two-upstream-audio-connection budget. Layout tests
  cover placement below details, centering, monochrome/small-screen fallbacks,
  and graphics passing through the terminal writer. Startup regressions cover
  early input, delayed replies, Escape and unknown control reports.
- Linux x86_64 musl packaging passes architecture/static-linkage checks and
  version/doctor smoke checks from the extracted archive. The archive includes
  nonempty guides, English completions, the manual and `BUILDINFO`.
- Release archives require the final GitHub CI gates for Linux, native macOS
  Apple Silicon and Intel, Rust 1.88 and RustSec auditing. CI collects all three
  archives with combined checksums only after those gates pass. Native package
  checks do not imply Developer ID signing, notarization or physical listening.

## 0.5.0 release candidate — 2026-10-20

- Linux all-target/all-feature tests: 171 passed, zero failures; the separate
  catalog benchmark is excluded from that count. Real mpv, FFmpeg/ffprobe, and
  pseudo-terminal tests use isolated data and local HTTP/ICY/HLS fixtures.
- Formatting, strict all-target/all-feature Clippy, and Rust 1.88 checks pass.
- The explicit release benchmark passes with 50,000 synthetic stations and
  search P95 50.04 ms, below its 100 ms absolute budget. This is a single local
  observation, not evidence of a comparative performance improvement.
- Local tools: Rust 1.97.1, mpv 0.40.0, FFmpeg/ffprobe 7.1.5 on Linux x86_64.
- Device-failure regressions cover synchronous and background playback, an
  immediately exiting mpv, preserved JSON v2 error codes/parameters, and a
  catalog station with backups: one loadfile, zero directory requests, no
  reconnect or random fallback. Background missing dependencies retain their
  original domain code. Real mpv covers valid and invalid output devices.
- Normalization tests cover real ICY titles, volume, and mute; concurrent starts
  share one capability probe, and separate executable paths keep separate
  cached results. Missing filters fail before playback; recording stays stream copy.
- TUI tests preserve border single clicks and constrain double-click playback.
  Selection-only restoration handles paging and missing targets once; the worker
  receives the same initial filter even if preferences change before it starts.
  Existing 80×24, 100×24, and 160×48 layout snapshots remain unchanged.
- Real terminal tests audition a URL without saving it and double-click a
  station to play. Recording tests freeze the starting title before synchronous
  or background probing, preserve filenames and metadata through finalization,
  and cover Unicode byte budgets and older records without a track field.
- These are Linux observations. The release gate also requires the GitHub CI
  jobs for Linux, macOS, Rust 1.88, x86_64 musl packaging, and RustSec auditing.
  Physical speaker listening is not part of automated validation.

验证日期：2026-10-17。环境：Debian Linux x86_64，内核 6.12.85，Intel Core i5-8365U（8 个逻辑 CPU），Rust 1.97.1、FFmpeg 7.1.3、mpv 0.40.0。release 构建开启 thin LTO，二进制约 7.61 MiB。

## 功能与工程检查

- `cargo fmt --check`、`cargo clippy --all-targets --locked -- -D warnings`。
- 27 项功能测试通过；另显式运行并通过五万条目录的性能测试。
- 自动测试覆盖目录分页与原子回滚、无效上游条目、中文与字面搜索、组合筛选、收藏保留、全目录随机抽样、CLI JSON 和配置错误。
- 实际 FFmpeg 验证 MP3、AAC、Vorbis、Opus 流拷贝及 HLS AAC；验证文件编码、时长、JSON 元数据、失败文件保留、提前 EOF 和中断恢复。磁盘写入错误使用失败后端模拟，未向真实文件系统灌满数据。
- 实际 mpv 使用 `--ao=null` 验证 ICY 标题、播放、暂停、音量、显式备用地址及起播超时。验证前台 SIGKILL 后子进程停止。
- 伪终端验证 TUI 渲染、调整窗口、`q`、Ctrl-C、SIGTERM，以及终端模式恢复。未进行物理扬声器听音验证。
- 新增 TUI 测试覆盖命令参数与引号、中文编辑与补全、历史草稿恢复、输入模式隔离、搜索取消、面板空间切换、窄布局鼠标焦点、列表按 UUID 保留选择和过时结果保护。
- 渲染测试验证亮青色粗边框、当前电台与选中项的信息分离、目录码率与未知值、当前播放请求地址、帮助与详情滚动边界；伪终端实际执行 `:sync`、添加电台、搜索、收藏、帮助与命令退出。
- 本地慢速分页测试验证同步期间音量命令仍可响应、重复同步报错、取消最后一页时不提交同步数据、之后重新同步成功并刷新列表。
- 本机 mpv 使用 Debian 软件包解压到 `target/tools/` 运行；没有修改系统软件安装。默认使用前仍需安装 mpv 或在配置中指定可用路径。

真实网络冒烟：通过默认服务器发现完成 Radio Browser 全目录同步，获取 62,160 条原始记录，过滤无效条目和去重后保存 **62,158 台**；随后验证中国电台的本地 JSON 搜索。测试目录位于 `target/smoke/`，未写入用户默认数据目录。公开目录数量随时间和服务器变化，不作为自动测试断言。

## 性能

五万条合成目录、100 次本地分页查询，混合中文、英文、数字、无匹配和特殊字符。最新一次搜索 P95 为 **19.70 ms**，低于 100 ms 目标。命令：

```sh
cargo test --release --test storage catalog_benchmark -- --ignored --nocapture
```

通过 `scripts/measure_resources.py` 测量相同规模目录、24×100 TUI、本地按 128 kbps 节奏发送的 MP3。音频输出为空设备；每种模式预热后采样 5 秒，RSS 为采样峰值，CPU 以一个逻辑 CPU 的 100% 为基准。

| 场景 | 进程 | RSS (MiB) | CPU |
|---|---|---:|---:|
| TUI 空闲 | radio | 12.49 | 0.20% |
| 播放 | radio | 12.74 | 0.00% |
| 播放 | mpv | 65.26 | 1.00% |
| 播放并录音 | radio | 12.66 | 0.20% |
| 播放并录音 | mpv | 65.40 | 2.19% |
| 播放并录音 | ffmpeg | 50.29 | 0.40% |

以上为命令模式和五面板界面更新后的测量。应用本体达到 RSS ≤30 MiB、空闲 CPU <1% 的目标；播放并录音的整套 RSS 约 **128.4 MiB**。资源脚本不计其自身和本地测试服务器。五秒采样受系统负载与 CPU 时间分辨率影响，0.00% 表示采样窗口未检测到 CPU tick 增量；实际音频设备、TLS、HLS 和电台编码会改变结果，不能据此承诺所有音源的总占用。

复现需要 mpv、FFmpeg 和 uv：

```sh
cargo build --release --locked
uv run scripts/measure_resources.py --binary target/release/radio
```

## 0.3.0 / S1–S3 validation — 2026-10-19

- All 101 default tests pass, including real mpv/FFmpeg and pseudo-terminal cases;
  the opt-in 50,000-station benchmark also passes. Tests use isolated data and
  local HTTP sources. A null audio backend exercises real IPC and media EOF.
- Coverage includes atomic bootstrap cancellation/rollback, literal Unicode
  search, station editing and duplicate URLs, a ten-second stream outage,
  persisted preferences, automatic sync, local recording playback/deletion,
  impossible-space refusal, versioned JSON and specific process exits.
- TUI snapshots cover list/details at 80×24, 100×24 and 160×48; every media-state
  pair renders, themes preserve focus, light colors contrast, and NO_COLOR
  emits no SGR commands in a pseudo-terminal.
- Shared-view tests assert cached Arc publications and collection sharing;
  station queries do not read corrupt unused recording rows. History retains
  precisely 1000 rows through an indexed threshold. Local benchmark repeats
  measured P95 26.26/21.65 ms; the immediate pre-performance-change baseline
  measured 20.57 ms. The test budget is 100 ms; these timings are environment
  observations, not a statistical claim of a specific speedup.
- Formatting, strict Clippy with all targets/features, Rust 1.88 checks and
  RustSec audit pass. CI installs real media executables and also verifies
  x86_64-unknown-linux-musl static linkage. No advisory ignore list is used.
- Shell generation covers bash/zsh/fish and radio(1) without creating data;
  packaging includes these assets, the lockfile hash and source provenance.

This release implements #36–#57. S4 (#58–#64) remains backlog. Static TUI/Clap
translation is the explicitly separate follow-up to the i18n foundation #42.

The extracted 0.3.0 archive also passes CLI/JSON/exits/doctor and real local mpv
playback-to-EOF/deletion smoke tests. Completion files and man output match the
binary; bash/zsh syntax checks and SHA-256 verification pass. Repackaging the same
commit with the same toolchain and built binary produces the same archive hash.
This is a repackaging check, not a claim of reproducible compilation across
different host toolchains. The internal roadmap is not archived.


## 0.3.1 / review follow-up — 2026-10-19 (release snapshot)

- At the 0.3.1 release snapshot, 118 tests passed with real mpv/FFmpeg,
  including two successive stream outages,
  real ffprobe bitrate preflight for a custom station with bitrate=0, active
  recording playback guards, coalesced refreshes and lazy backup reads.
- Strict all-target/all-feature Clippy, formatting, Rust 1.88 and a fresh RustSec
  audit pass. Candidate filtering, narrow detail navigation, short wide headers,
  observed mute state and localized CLI progress have regression coverage.
- Release packaging uses a clean tracked checkout, fresh staging and an explicit
  guide whitelist (`tui.md`, `json-contract.md`, `validation.md`), and rejects
  empty generated files. The musl CI job runs this packaging path.

### Comparative performance acceptance

The original #55 condition “no P95 regression” was **not demonstrated** by the
single 20.57 ms baseline versus 26.26/21.65 ms release observations. Being below
100 ms alone does not establish comparative non-regression.

Repeated the unchanged 50,000-station release benchmark on the same host/toolchain
(Rust 1.97.1, Linux 6.12.85+deb13-amd64). Compared the immediate pre-#55 commit
`1dea93b` with follow-up runtime code `4bcf9e9`; subsequent changes affect only recording text, version and documentation. Each process creates the same synthetic catalog shape,
then measures 100 queries across five query classes. Discarded one warm-up run
per binary and alternated AB/BA order over ten pairs, without concurrent builds.
UUIDs and host scheduling are not fixed; this is a host observation, not an
exclusive-hardware experiment. Each cell is that run's search P95 in ms.

| Pair | Before #55 | Review fixes |
| --- | ---: | ---: |
| 1 | 23.64 | 26.37 |
| 2 | 26.56 | 26.81 |
| 3 | 28.63 | 27.12 |
| 4 | 27.42 | 27.50 |
| 5 | 28.10 | 27.50 |
| 6 | 21.88 | 25.47 |
| 7 | 23.16 | 22.30 |
| 8 | 22.14 | 23.77 |
| 9 | 22.10 | 22.21 |
| 10 | 24.93 | 21.31 |

The median of the ten P95 values is **24.285 → 25.920 ms (+6.7%)**; the means
are **24.856 → 25.036 ms (+0.7%)**. Ranges overlap (21.88–28.63 versus
21.31–27.50 ms). These observations do not prove either strict equivalence or a
statistically established regression, and no speedup is claimed.

Acceptance clarification: **accept the observed small median increase**, rather
than marking the original comparative gate as proven. The worst follow-up run
is 27.50 ms against the separate 100 ms budget, while the targeted TUI costs
(double refreshes, per-row backup reads, full snapshot cloning and unused-source
reads) are removed and covered by structural/behavioral tests. Preserve these
numbers for future comparison instead of attributing all differences to noise.

Reproduce each sample with the corresponding release test executable:

```sh
cargo test --release --test storage catalog_benchmark -- --ignored --nocapture
```

## 0.4.0 validation — 2026-10-19

- `cargo test --all-targets --all-features --locked`: **139 passed**, with the
  opt-in catalog benchmark excluded (one ignored). Real mpv/FFmpeg and terminal
  tests ran; sources and data directories remain local and isolated.
- `cargo fmt --check`, strict all-target/all-feature Clippy and
  `cargo +1.88.0 check --all-targets --all-features --locked` pass.
- Backup-query regressions cover 2/4/8/16/30-second retry backoff, unchanged-error
  log/redraw suppression, visible error details, recovery and cache invalidation.
- Playing-station location from a randomly sorted search uses the stable default
  order for both rank and page queries. Empty shortcut bindings, incomplete named
  keys and shifted control input have regression coverage.
- Real truncated AAC streams preserve reconnect counts in SQLite and sidecars;
  finalization recovery retains the count. CLI/TUI details warn about possible
  gaps. CLI text lists retain a diagnostic tail beyond 1024 characters while
  stripping terminal control characters.
- The real two-outage playback test also passes with `LANG` and `LC_ALL` set to
  `zh_CN.UTF-8`; it observes playback states instead of localized status text.
- Release manual generation with `--lang en` produces identical bytes under
  English/default and Chinese host language settings, including `RADIO_LANG`.
  Release completion generation is also pinned to English and covered by all
  three shells. A real local HLS fixture with a missing segment records an
  interrupted result, preserves diagnostics, and counts the failure as a
  reconnect. Recording lists retain tab-separated diagnostics and mark rows
  whose reconnect count is positive. `bash -n scripts/package-release.sh`
  passes. A fresh archive reproducibility comparison was not run for this
  review.
