# radio-cli

[![CI](https://github.com/hmgle/radio-cli/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/hmgle/radio-cli/actions/workflows/ci.yml)
[![最新版本](https://img.shields.io/github/v/release/hmgle/radio-cli.svg?logo=github)](https://github.com/hmgle/radio-cli/releases/latest)
[![Rust 1.88+](https://img.shields.io/badge/Rust-1.88%2B-dea584.svg?logo=rust)](Cargo.toml)
[![Linux 与 macOS](https://img.shields.io/badge/platforms-Linux%20%7C%20macOS-blue.svg)](#安装与启动)
[![MIT 许可证](https://img.shields.io/badge/license-MIT-green.svg)](LICENSE)

[English](README.md) | **简体中文**

Linux 优先的轻量 CLI / TUI 网络收音机。Rust 管理本地目录与交互，SQLite 保存收藏和历史，mpv 播放，FFmpeg 无损录音。无需账号或 API Key。

Linux 是主要支持平台。macOS 支持基本 CLI/TUI 操作：目录管理、收藏和历史、播放控制、
录音及正常退出。代码依赖 Unix API，Windows 原生暂不支持。CI 在 Linux 和 macOS 上
使用真实 mpv、FFmpeg 测试；Linux 检查还覆盖 Rust 1.88、x86_64 musl 静态构建
及 RustSec 依赖审计。

## 演示

[![观看 radio-cli 浏览 CN 电台并播放真实广播的演示](assets/demo.svg)](https://asciinema.org/a/1269102)

[在 asciinema 观看 90 秒演示](https://asciinema.org/a/1269102) · 中文界面与字幕 · zsh · 无声音。

展示已同步目录的浏览、CN 地区筛选、搜索与详情、收藏、播放控制、当前电台定位、
最近播放，以及简短录音与回放。

## 安装与启动

需要 mpv、FFmpeg（含 ffprobe）；从源码构建还需要 Rust 1.88+。
SQLite 已编入程序，不要求安装 SQLite 命令行工具。

[发布页面](https://github.com/hmgle/radio-cli/releases)提供预编译程序和统一的 `SHA256SUMS`。
使用预编译包无需 Rust 工具链，仍需单独安装 mpv 和 FFmpeg。升级兼容性变化见[更新日志](CHANGELOG.md)。

| 平台 | 归档目标 | 支持状态 |
| --- | --- | --- |
| Linux x86_64 | `x86_64-unknown-linux-musl` | 主要支持；静态链接 |
| macOS Apple Silicon | `aarch64-apple-darwin` | 实验性 |
| macOS Intel | `x86_64-apple-darwin` | 实验性 |

```sh
# Debian / Ubuntu
sudo apt install mpv ffmpeg
cargo build --release --locked
./target/release/radio doctor
./target/release/radio sync
./target/release/radio tui

# 或安装到 ~/.cargo/bin
cargo install --path . --locked
```

macOS 先通过 Homebrew 安装媒体工具，再下载匹配架构的归档和 `SHA256SUMS`，
校验后解压。以 Apple Silicon 为例：

```sh
brew install mpv ffmpeg
grep 'aarch64-apple-darwin.tar.gz$' SHA256SUMS | shasum -a 256 -c -
tar -xzf radio-cli-v0.7.0-aarch64-apple-darwin.tar.gz
cd radio-cli-v0.7.0-aarch64-apple-darwin
./radio doctor
./radio sync
./radio tui
```

Intel Mac 请替换为 `x86_64-apple-darwin`。macOS 包未经 Developer ID 签名或公证。
若 Gatekeeper 阻止运行你信任的下载文件，校验通过后，可对解压出的程序执行
`xattr -d com.apple.quarantine ./radio` 再重试；另见
[Apple 关于打开下载软件的说明](https://support.apple.com/en-us/102445)。
也可安装 Rust 1.88+，在源码目录运行 `cargo install --path . --locked`。

直接运行 `radio` 会在交互终端打开 TUI；重定向输出时显示帮助。没有目录时，TUI 和 `random` 先获取热门 1,000 台；完整目录通过 `sync` 获取。同步失败保留之前的目录。

## 命令

```sh
radio search jazz --country FR --min-bitrate 128
radio search 新闻 --language chinese
radio search --tag 'public radio' --limit 100 --offset 100
radio --json search jazz
radio info <UUID>
radio play <UUID>
radio play https://example.com/live.mp3
radio random --country CN
radio favorites add <UUID>
radio favorites list
radio favorites remove <UUID>
radio history
radio station add '我的电台' 'https://example.com/live' \
  --backup-url 'https://example.com/backup'
radio record <UUID> --duration 30m --output-dir ./recordings
radio recordings list
```

名称、国家/地区、语言和标签支持本地搜索；两个字符及以上的查询还会按子串匹配国家/地区的中文名称，
例如 `刚果` 可同时命中两个刚果的电台，`法国` 可命中法国电台。单字符查询只匹配电台文本，因为单个汉字被过多地区共用
（`岛` 出现在 32 个名称中）。国家/地区代码、语言和标签筛选使用完整值（语言也可用目录提供的代码，例如 `zh`）。最低码率筛选排除码率未知的记录。`random` 在整个匹配目录中抽样，不受搜索分页限制。`info` 始终输出 JSON；查询命令可加 `--json`。运行状态与诊断写入 stderr。

`--json-v2` 返回统一的成功/错误封套和稳定错误码，原有 `--json` 格式保持兼容，
详见 [JSON 契约](docs/json-contract.md)。录音支持 `recordings play ID`、
`recordings remove ID [--index-only]`、`recordings open`；TUI 中 Enter / D / o / e
分别回听、确认删除、打开目录和展开错误。录音空间检查默认保留 16 MiB。

电台采用完整 Radio Browser UUID；自定义电台创建本地 UUID。收藏、最近播放和自定义地址不被目录同步覆盖。收藏中已从目录消失的电台仍可尝试播放。历史保留最近 1,000 次成功起播记录，界面按电台聚合显示最近收听。

TUI 按键：

| 按键 | 操作 |
|---|---|
| `:` | 命令模式；Enter 执行，Esc 返回 |
| Tab / Shift-Tab | 下一个 / 上一个面板 |
| `1`–`5` | 来源 / 列表 / 详情 / 播放器 / 任务面板 |
| Ctrl-方向键 / Ctrl-`h/j/k/l` | 按空间位置切换面板 |
| 方向键 / `j`、`k` | 选择、导航或滚动当前面板 |
| Enter | 列表中播放；播放器中查看当前电台详情 |
| Space / `m` | 暂停 / 恢复 / 静音，录音继续 |
| `y` / `Y` | OSC 52 复制 UUID / URL（录音 ID / 路径） |
| `n` | 按当前筛选随机下一台 |
| `f` | 收藏 / 取消收藏选中电台 |
| `r` | 开始 / 停止当前电台录音 |
| `+`、`-` | 音量 |
| `/` | 增量搜索；Enter 确认，Esc 恢复原查询、页和选中项 |
| `F` | 国家/地区代码、语言、标签、最低码率筛选 |
| `[` / `]` | 电台 / 收藏 / 最近 / 录音分类；会话内各分类记住自己的页和选中行 |
| `gg` / `G` | 列表首项 / 末项 |
| PgUp / PgDn、Ctrl-b / Ctrl-f | 电台列表翻页 |
| Ctrl-e / Ctrl-y | 在当前面板向下 / 向上移动一行 |
| `?` | 打开帮助或当前失败详情（详情/命令用法内按 `h` 切换到键盘帮助；`j`/`k`、PgUp/PgDn、`g`/`G`、Ctrl-e/y/b/f 滚动） |
| `q` / Ctrl-C | 收尾录音并退出 |

焦点面板采用亮青色粗边框、`▶` 标记和编号，底部显示模式与当前面板操作；也支持鼠标点击切换面板、选择行，双击电台行播放，滚轮导航。顶部持续显示当前播放电台、编码、码率、音量和录音状态，详情面板展示国家/地区、语言、标签、UUID、地址等信息。码率与编码标注为目录数据，未知值明确显示未知；不会随列表选择而改变当前播放信息。

在 TUI 内直接执行 `:sync` 后台同步，`:sync-cancel` 取消任务，`:current` 查看当前电台完整详情，`:record --duration 30m` 定时录制当前电台。命令支持引号、Tab 补全和历史；更多命令与操作设计见 [TUI 使用说明](docs/tui.md)。

`play` 和 TUI `:play URL` 也接受 HTTP(S) 流地址。直接试听不会写入目录或播放历史，
也不会替换上次播放电台；需要录音时请先用 `station add` 保存。

## 配置与数据

默认电台目录通过第三方服务 [Radio Browser API](https://api.radio-browser.info/) 获取，
并缓存在用户本地 SQLite 数据库中。源码仓库及发行包不附带电台目录数据库。
目录元数据由上游提供；使用自定义 `api_url` 时，目录来自指定服务。

中文 TUI 的国家/地区名称根据上游 `countrycode`，使用固定版本的
[Unicode CLDR 48](https://github.com/unicode-org/cldr/tree/release-48) 中文名称显示；代码无法识别或缺失时回退显示上游目录名称，再回退代码，最后显示未知。
`state` 字段标为“州/省/区域”。
上游 `country`、`countrycode` 字段在本地保存和 JSON 输出中保留原值，
`--country` 参数及 `countries` 命令保持兼容，代码筛选仍使用上游代码。
地理名称与代码仅用于电台检索和展示，不表示本项目对相关地区政治地位或主权归属的立场。
CLDR 名称表使用 [Unicode License V3](locales/LICENSE.unicode)，
可用 `uv run scripts/update_territories.py` 从固定上游版本重新生成。

CLI 错误和列表文案支持 `--lang en` / `--lang zh-CN`。优先级为 `--lang`、
`RADIO_LANG`、配置 `language`、`LC_ALL`/`LANG`，未知语言提示一次并回退英文。
CLI 列表、领域错误、播放进度、顶层帮助简介以及 TUI 面板、帮助和交互提示已支持
语言切换；子命令帮助和部分运行消息仍待迁移。

自定义电台支持 `station list --all`、`station edit UUID --name 名称 --url URL`、
`station remove UUID`。删除有引用的电台时保留名称占位并清除播放地址；
`--keep-references` 可强制保留无引用占位。`station backups UUID --add URL --remove URL`
管理备用地址，归一化后重复的 URL 会报出已有 UUID。TUI 删除需按 `y` 二次确认。

已起播的直播流遇到 EOF/错误时，按 1、3、7 秒退避最多重连三次，保持电台和当前地址，
不会重复记入历史或点击上报。停止、换台、退出会取消待重试任务；耗尽后进入正常备用线路流程。
恢复播放后重新获得每次断流的三次预算；滑动窗口限制每分钟最多重连六次，防止抖动流无限重试。
播放开始前的流连接失败仍按候选线路回退。播放器本地启动失败（缺少可执行程序、
IPC/socket 启动超时、缺少响度滤镜）及配置的音频设备失败会结束本次播放请求，
不尝试其他线路、随机电台或目录刷新。解决本地问题后重新播放；偶发的播放器启动失败也需重新发起播放。

`radio devices`（或 `radio --json devices`）列出 mpv 音频输出设备；
设置 `audio_device = "设备名"` 选择输出，留空或省略则自动选择。
设备名取决于 mpv 的音频后端与本机硬件，重新连接硬件后可能变化。

`normalize_loudness = true` 可开启自适应播放响度均衡，默认关闭。
使用 mpv 的 `dynaudnorm` 短窗口（`f=150:g=5`），会增加少量缓冲；
录音仍原流复制，不受影响。缺少滤镜时会提示安装兼容的 mpv 或关闭此选项。
每个进程按配置的 mpv 可执行程序只探测一次滤镜能力；`radio devices` 始终实时枚举设备。

运行时音量保存在 SQLite；配置 `resume = true` 可在 TUI 启动时恢复上次成功播放的电台和来源，
默认关闭自动恢复播放。不会修改手写配置文件；无效音量回退配置值，已不可用的上次电台跳过。
`resume = "selection"` 只恢复来源与上次电台的选择（含所在分页），不启动声音；
电台不在该来源中时选中首行，录音来源不恢复电台选择。`false` 则打开全部电台且不播放。

`countries`、`languages --prefix zh`、`tags --prefix jazz` 列出本地实际筛选值及电台数。
搜索支持 `--sort name|bitrate|votes|clickcount|random`，默认按点击数，能与全部筛选组合；
随机排序每次查询重新排列。TUI 的 `F` 弹窗后台加载候选，↑/↓ 选择并填入。

TUI 启动时自动后台刷新已完整同步且超过 `auto_sync_days` 天的目录（默认 7，0 关闭），
自动同步也保留取消与缩减保护。`F` 弹窗 Ctrl-U 或点击底栏明确的清空筛选标签可清空筛选。
查询仍按字面子串匹配，多词 CLI 参数用 shell 引号分组；`-tag:` 等高级语法留待后续。

TUI 普通模式下连按 `''`（两次单引号，间隔不超过 1 秒）可定位当前播放电台并聚焦列表。
快捷键在 `[keybindings]` 中配置 `jump_to_playing`（如 `"gp"`、`"<F6>"`、`"<Ctrl-p>"`），
支持 1–4 个可打印非空白字符或带 `Ctrl-`/`Alt-`/`Shift-` 修饰的命名键；Ctrl-C 保留退出，
设为 `""` 可禁用。目标在当前页时保留来源、筛选和排序，否则切换到全部电台、清除筛选并
只加载目标所在页；定位不重新播放，暂停、连接和重连期间同样可用，播放失败后可定位到头部
所示电台并提示“播放失败”，空闲时提示无播放。帮助面板
显示实际配置的按键，禁用时不显示该条目。详见 [docs/tui.md](docs/tui.md)。

Linux 默认路径：

- 配置：`${XDG_CONFIG_HOME:-~/.config}/radio-cli/config.toml`
- 数据：`${XDG_DATA_HOME:-~/.local/share}/radio-cli/radio.db`
- 录音：数据目录下的 `recordings/`

macOS 默认路径：

- 配置：`~/Library/Application Support/radio-cli/config.toml`
- 数据：`~/Library/Application Support/radio-cli/radio.db`
- 录音：`~/Library/Application Support/radio-cli/recordings/`

配置文件可选。参见 [config.example.toml](config.example.toml)，使用 `--config 文件` 指定；`--data-dir 目录` 可隔离测试数据。相对目录按启动时的工作目录解释；录音数据库保存绝对路径。

默认发现 Radio Browser 服务器并有限重试。可设置 `api_url`，或使用优先级更高的 `--api-url https://服务器`。HTTP 请求支持 `HTTP_PROXY`、`HTTPS_PROXY`、`ALL_PROXY`、`NO_PROXY` 等标准环境变量；音频后端遵循其各自的代理支持，目录可达不代表音频流可达。过期目录按 auto_sync_days 配置自动后台刷新，可设 0 关闭。

同一数据目录只允许一个播放 / 录音 / TUI 会话；其他 CLI 查询和收藏操作仍可使用。Linux 和 macOS 正常退出或 Ctrl-C 时均停止播放并收尾录音。Linux 还会在前台被强制终止时结束媒体子进程；macOS 不保证 SIGKILL 或崩溃后的子进程清理。长时间运行可放在 tmux 中，Linux 也可使用 systemd。

## 录音行为

播放时开始录音，会将请求录音时已知的曲名保存到文件名和元数据；切歌不会改名。
未知曲名（含独立 CLI 录音）沿用原命名格式，文件名会清理特殊字符并限制字节数，
曲名词语间的空白折叠为下划线。

- MP3 → `.mp3`；AAC（包括常见 HLS AAC）→ `.aac`；Vorbis → `.ogg`；Opus → `.opus`。根据实际音轨探测，不盲信目录的编码字段。
- 使用流拷贝，不重编码、不降低音质；不支持的录音编码明确报错。
- 每个录音有 JSON 元数据，包含电台、实际请求地址、时间、编码、媒体时长和完成状态。源站重定向后的最终 CDN 地址不一定可见。
- 先写唯一 `.part` 文件；正常停止后生成最终文件。中断 / 失败保留 `.part` 和状态，后续启动媒体会话时恢复未收尾记录。`recordings list` 可查看结果；媒体时长可能为估计值。
- 最终文件不会覆盖已有文件。macOS 的录音发布要求文件系统支持硬链接；发布中断后可使用 `recordings recover` 重试。
- TUI 切台先结束当前录音；新电台需重新按 `r`。暂停播放不会暂停录音。直播源提前结束不视作完整录音。
- HTTP/ICY 连接异常截断时有限重连，HLS 分段拉取失败也计入重连次数。重连可能留下直播音频缺口；完成的录音仍保存重连次数并显示提示。重试耗尽仍保留临时文件和错误诊断。不会循环播放有限长度的源来凑足录音时长。
- `--duration` 接受秒、`s`、`m`、`h`，范围为 1 秒至 365 天。切点以音频包为单位，不能保证精确歌曲边界。
- 边听边录最多两条并发上游连接；编码探测连接先关闭，再启动录音连接。HLS 一个媒体任务可能请求多个分片。
- 文件只保存在本地；请遵守电台条款与适用的个人录制规定。

如需恢复 `.part`，先复制到新文件，再根据原编码用 FFmpeg 流拷贝。不要直接覆盖原文件。

## 开发与验证

```sh
cargo fmt --check
cargo clippy --all-targets --all-features --locked -- -D warnings
cargo test --all-targets --all-features --locked
cargo test --release --test storage catalog_benchmark -- --ignored --nocapture
# 仅 Linux：资源采样依赖 /proc
uv run scripts/measure_resources.py --binary target/release/radio
```

测试使用本地 HTTP/ICY/HLS 源，不依赖公共电台。录音测试需要 FFmpeg；mpv IPC 测试使用空音频输出，可通过 `RADIO_TEST_MPV=/绝对路径/mpv` 指定，未找到 mpv 时会跳过相关测试。CI 安装并执行真实 mpv 测试。资源目标与结果见 [验证记录](docs/validation.md)。

首版不提供后台服务、回溯缓存、歌曲识别、歌词、地图和后台全目录探测。目录来源为 [Radio Browser](https://www.radio-browser.info/)，音频直连电台；不使用 RoamDial 私有服务，也不会按同名自动合并备用线路。更详细的设计见 [实现说明](docs/implementation.md)。

Ctrl-C 在所有 TUI 模式下收尾并退出；Esc 取消编辑或关闭弹窗。终端过小时，q 直接退出。

`radio completions bash|zsh|fish` 生成 shell 补全，例如
`radio completions zsh > _radio` 后放入 zsh 的 `$fpath` 目录。
`radio man > radio.1` 生成手册页。发布压缩包包含三种 shell 补全和
`man/radio.1`，生成命令不读取配置、不创建数据目录。

`theme = "default"|"mono"|"light"` 设置主题，非空 `NO_COLOR` 强制 mono。
可设置 `artwork = "station"` 在 TUI 详情中显示电台台标，或用 `"track"` 优先显示
当前曲目的 ICY 封面，默认 `"off"`。
台标来自目录的 `favicon`，不探测音频流；升级后运行 `radio sync` 为旧电台补齐数据。
支持的终端使用 Kitty、Sixel 或 iTerm2 图片协议，否则回退 Unicode 彩色半块；
tmux 固定使用半块。图片利用详情文字下方的剩余空间，封面切换时文字位置不变。
单色主题、小于 100×24 的窗口或文字下方空间不足时保留纯文字详情。
部分 Sixel 终端（包括 xterm）的图片被弹窗或文字覆盖后可能消失，可尝试重新打开详情。
仅下载不超过 256 KiB 的 JPEG/PNG，并限制解码尺寸；图片缓存在数据目录的
`artwork/` 中，最多 128 项、32 MiB，台标 URL 变化后重新获取。
曲目封面要求 ICY `StreamUrl` 是有效图片地址，且元数据标题与当前非空播放标题一致；
不可用时回退台标或文字。仅在详情展示的电台正在播放时探测，间隔 25 秒，
可能被源站计为额外听众；暂停、停止、切台和退出会取消探测，HLS 和本地录音回放除外。
录音优先：等待封面探测连接关闭后才开始检测编码，检测、录制和收尾期间回退台标。
等待关闭超过 `connect_timeout_secs` 的两倍时，播放或录音启动会提示超时，可重试；
不会因此超出连接数上限。
播放和录音仍最多使用两条音频上游连接；目录与图片 HTTP 请求另计。
曲目关联信息不持久化；下载的图片文件可复用缓存。
80×24 下详情用 `3` 打开，空闲任务栏收起。`mouse = false` 或 `:set mouse false`
允许终端选择文本。命令历史跨会话保留 100 条（含 URL 凭据的命令不保存）；`:help record` 查看命令用法，
`:set language en|zh-CN` 在当前会话切换已迁移的消息语言。

录音空间预检优先使用探测到的音频流码率，再使用容器码率、目录码率。无时限或码率未知时，只检查配置的最低余量并提示无法估算总大小；预检无法保证录音途中磁盘不会耗尽。
