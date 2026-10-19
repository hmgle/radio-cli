# 验证记录

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

## Unreleased review validation — 2026-10-19

- `cargo test --all-targets --all-features --locked`: **133 passed**, with the
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
  `bash -n scripts/package-release.sh` passes. A fresh archive reproducibility
  comparison was not run for this review.
