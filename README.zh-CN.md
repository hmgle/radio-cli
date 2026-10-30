# radio-cli

[![CI](https://github.com/hmgle/radio-cli/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/hmgle/radio-cli/actions/workflows/ci.yml)
[![最新版本](https://img.shields.io/github/v/release/hmgle/radio-cli.svg?logo=github)](https://github.com/hmgle/radio-cli/releases/latest)
[![Crates.io](https://img.shields.io/crates/v/radio-cli.svg?logo=rust)](https://crates.io/crates/radio-cli)
[![npm](https://img.shields.io/npm/v/@hmgle/radio-cli.svg?logo=npm)](https://www.npmjs.com/package/@hmgle/radio-cli)
[![Rust 1.88+](https://img.shields.io/badge/Rust-1.88%2B-dea584.svg?logo=rust)](https://github.com/hmgle/radio-cli/blob/main/Cargo.toml)
[![Linux 与 macOS](https://img.shields.io/badge/platforms-Linux%20%7C%20macOS-blue.svg)](#环境要求)
[![MIT 许可证](https://img.shields.io/badge/license-MIT-green.svg)](LICENSE)

[English](README.md) | **简体中文**

面向 Linux 与 macOS 的轻量终端收音机。Rust 管理本地目录和交互，SQLite 保存收藏与历史，
mpv 负责播放，FFmpeg 以流拷贝方式录音。无需账号或 API Key。

- 浏览 [Radio Browser](https://www.radio-browser.info/) 目录，在本地搜索，
  按国家/地区、语言、标签和码率筛选，并随机播放匹配电台。
- 使用键盘优先的 TUI，或用 CLI 编写脚本；支持兼容旧格式的 `--json` 和稳定的
  `--json-v2` 输出。
- 收藏、历史、自定义电台、设置和录音都保存在本机。
- 可选本地直播字幕，支持 SenseVoice 或 Qwen3；见[字幕安装](docs/configuration.zh-CN.md#直播字幕)。
- 可播放任意 HTTP(S) 流地址，或保存带备用地址的自定义电台。
- 以流拷贝方式录制 MP3、AAC、Vorbis 和 Opus，并生成元数据。

Linux 是主要支持平台。macOS 支持核心 CLI/TUI 流程。代码依赖 Unix API，
暂不支持原生 Windows。

## 演示

[![观看 radio-cli 浏览 CN 电台并播放真实广播的演示](assets/demo.svg)](https://asciinema.org/a/1269102)

[在 asciinema 观看 90 秒演示](https://asciinema.org/a/1269102)。演示展示中文界面
浏览已同步目录、按 `CN` 筛选、搜索、播放、收藏、定位当前电台以及录制短片的流程；
录像无声音，展示的是较早的界面；当前版本的快捷键提示已有更新。

## 快速开始

最快的安装方式是 npm 包：

```sh
npm install -g @hmgle/radio-cli
radio doctor
radio sync
radio tui
```

运行时依赖为 `mpv`、`ffmpeg` 和 `ffprobe`。也可以从 crates.io 安装：

```sh
cargo install radio-cli --locked
```

平台归档校验、crates.io 安装、源码构建、macOS Gatekeeper 说明和首次运行行为
详见[安装指南](docs/installation.zh-CN.md)。

## 日常使用

### TUI 快捷键

| 场景 | 按键 | 操作 |
| --- | --- | --- |
| 通用 | `q` / `Ctrl-C` | 收尾录音并退出 |
| 通用 | `?` | 键盘帮助；有未处理的失败时先显示失败详情 |
| 通用 | `Esc` | 关闭弹窗、取消编辑或忽略错误 |
| 通用 | `F5` | 刷新当前列表 |
| 面板 | `Tab` / `Shift-Tab` | 下一个 / 上一个面板 |
| 面板 | `1`–`5` | 聚焦来源 / 列表 / 详情 / 播放器 / 任务面板 |
| 面板 | `h` / `l`、方向键 | 左右移动焦点，或选择、滚动 |
| 面板 | `Ctrl-h/j/k/l`、`Ctrl` + 方向键 | 按屏幕方向移动焦点 |
| 来源 | `[` / `]` | 上一个 / 下一个来源：电台、收藏、最近、录音 |
| 来源 | `j` / `k`、方向键 | 聚焦来源面板时切换到下一个 / 上一个来源 |
| 列表 | `j` / `k`、方向键 | 移动选择 |
| 列表 | `gg` / `G`、`Home` / `End` | 首项 / 末项 |
| 列表 | `Ctrl-u` / `Ctrl-d` | 移动十项 |
| 列表 | `H` / `M` / `L` | 选择视口顶部 / 中间 / 底部行 |
| 列表 | `zt` / `zz` / `zb` | 将选中行放到视口顶部 / 中间 / 底部 |
| 列表 | `PgUp` / `PgDn`、`Ctrl-b` / `Ctrl-f` | 上一 / 下一数据页；其他面板中滚动十行 |
| 列表 | `Ctrl-e` / `Ctrl-y` | 视口向下 / 向上滚动一行 |
| 播放 | `Enter` | 播放选中电台或录音；来源面板中进入列表；播放器面板中查看播放中电台详情 |
| 播放 | `Space` | 暂停 / 继续 |
| 播放 | `m` | 静音 / 取消静音 |
| 播放 | `+` 或 `=` / `-` | 提高 / 降低音量 |
| 播放 | `n` | 按当前筛选随机播放 |
| 播放 | `f` | 收藏 / 取消收藏选中电台 |
| 播放 | `r` | 开始 / 停止录制当前播放电台 |
| 复制 | `y` / `Y` | 复制选中 UUID / 流地址，或录音 ID / 路径 |
| 搜索 | `/` | 搜索；`Enter` 确认，`Esc` 恢复原视图 |
| 搜索 | `F` | 国家/地区、语言、标签和最低码率筛选 |
| 搜索 | `Ctrl-U` | 筛选弹窗内清空搜索与筛选 |
| 录音 | `D` / `o` / `e` | 删除（`y` 确认；`Enter` / `n` / `Esc` / `q` 取消）/ 打开目录 / 显示错误详情 |
| 定位 | 可配置 | 定位正在播放的电台；默认一秒内连按两次 `''` |

底栏中高亮的 token 表示需要按下的按键。例如 `[ / ]` 表示两个独立按键
`[` 和 `]`，中间的斜杠只是分隔符。

鼠标默认启用：可点击面板或行，500 ms 内双击行播放，用滚轮导航。

录音来源不提供目录搜索：`/`、`F` 和收藏键在该来源中不可用；按 `n` 会切回电台列表并随机播放。
tmux 会把 `Ctrl-b` 当作前缀键截获，翻页请改用 `PgUp` / `PgDn`。
弹窗、文本输入和删除确认使用各自的按键；在 TUI 内按 `?` 查看完整的上下文帮助。
列表查询失败后会自动退避重试，间隔从 500 ms 指数增长到 30 秒；`F5` 可绕过等待。
普通一次性提示约 4 秒后自动消失。`y` 和 `Y` 通过 OSC 52 复制，是否生效取决于终端和复用器；
超过 4,096 字节会截断并提示。

### TUI 内置命令

| 命令 | 操作 |
| --- | --- |
| `:sync [--accept-shrink]` / `:sync-cancel` | 启动或取消后台目录同步 |
| `:refresh` | 刷新当前列表 |
| `:search [QUERY] [OPTIONS]` | 使用 CLI 搜索选项应用搜索 |
| `:play [UUID\|URL]` | 播放选中项、指定 UUID 或临时流地址 |
| `:random [OPTIONS]` | 随机播放；不带参数时沿用当前筛选 |
| `:info [UUID]` / `:current` | 查看指定电台 / 当前播放电台 |
| `:favorites list\|add\|remove [UUID]` | 管理收藏 |
| `:history` | 显示最近成功播放记录 |
| `:recordings list\|play ID\|remove ID [--index-only]` | 查看、播放或删除录音 |
| `:open recordings` | 打开录音目录 |
| `:station add\|list\|edit\|remove\|backups ...` | 管理自定义电台和备用地址 |
| `:record [UUID] [--duration D] [--output-dir DIR]` / `:record stop` | 开始或收尾录音 |
| `:pause` / `:mute` / `:stop` / `:volume 70` | 控制播放 |
| `:help [COMMAND]` | 打开键盘帮助或命令用法 |
| `:set language en\|zh-CN` / `:set mouse true\|false` | 修改当前会话设置 |
| `:doctor` / `:focus sources\|list\|details\|player\|activity` | 运行诊断 / 移动焦点 |
| `:subtitles on\|off\|toggle\|history\|clear\|retry` | 控制直播字幕 / 查看会话历史 |
| `:set subtitles.model ID` | 切换当前会话字幕模型 |
| `:quit` / `:q` | 收尾录音并退出 |

`:help` 显示完整的上下文键盘帮助；输入 `:` 后按 `Tab` 可补全命令。
`:record UUID` 只接受当前正在播放的电台；需要独立录音时请使用
`radio record UUID`。

### 地图探索

在来源中选择 **地图探索** 或执行 `:explore`。宽屏并排显示离线世界地图和电台列表；窄屏按 `6` 查看地图、`2` 查看列表、`3`
查看详情。点击一个地图字符格即可列出其中电台。地图聚焦时，使用方向键或 `hjkl` 移动光标，`z` / `Z` 放大或缩小（最高 8×），`0`
返回全球，`Enter` 转到结果列表。地图上的滚轮以指针位置为中心缩放。播放和音量快捷键保持原有含义。

陆地底图和电台网格统一使用小圆点；灰色表示陆地，蓝、青、橙、红分别表示每格 1–9、10–49、50–199、200+ 个电台，地图下方显示颜色图例。
光标通过反显标记选中格，不再放大圆点。单色模式通过下划线区分电台点，并保留选中格的电台数量，不添加颜色。

搜索和筛选同时作用于地图与结果。按 `c` 按国家地区浏览无坐标电台，按 `Esc` 返回地图。升级后运行 `radio sync`
获取坐标；旧目录未保留这些字段。地理元数据由电台提交，可能不准确。内嵌的 Natural Earth 110m
陆地轮廓属于公有领域，不请求在线瓦片；小岛和放大后的海岸线可能较为简化。

### CLI 命令参考

以下全局选项适用于所有子命令：

| 选项 | 说明 |
| --- | --- |
| `--lang <en\|zh-CN>` | 选择界面和 CLI 语言 |
| `--config <FILE>` | 使用指定的 TOML 配置文件 |
| `--data-dir <DIR>` | 覆盖数据目录 |
| `--api-url <URL>` | 覆盖 Radio Browser 服务器发现结果 |
| `--json` | 输出旧版 JSON 结构 |
| `--json-v2` | 输出统一的成功/错误封套 |
| `-h`, `--help` | 打印帮助 |
| `-V`, `--version` | 打印版本 |

目录与搜索命令：

| 命令 | 用途 |
| --- | --- |
| `radio` / `radio tui` | 打开 TUI |
| `radio sync [--accept-shrink]` | 获取并替换完整目录 |
| `radio search [QUERY] [OPTIONS]` | 搜索本地目录 |
| `radio random [QUERY] [OPTIONS]` | 随机播放一个匹配电台 |
| `radio info <UUID>` | 以 JSON 输出单个电台 |
| `radio countries [--prefix TEXT]` | 列出国家/地区代码及数量 |
| `radio languages [--prefix TEXT]` | 列出语言及数量 |
| `radio tags [--prefix TEXT]` | 列出标签及数量 |

`search` 和 `random` 接受 `--sort name|bitrate|votes|clickcount|random`、
`--country CODE`、`--language LANGUAGE`、`--tag TAG`、`--min-bitrate KBPS`、
`--limit N`（1–1000，默认 100）和 `--offset N`。搜索默认按 `clickcount` 排序。
`info` 始终输出电台 JSON。

收藏、历史与自定义电台：

| 命令 | 用途 |
| --- | --- |
| `radio favorites list [PAGE]` | 列出收藏 |
| `radio favorites add <UUID>` | 添加收藏 |
| `radio favorites remove <UUID>` | 取消收藏 |
| `radio history [PAGE]` | 显示成功起播历史 |
| `radio station list [PAGE]` | 列出自定义电台 |
| `radio station add <NAME> <URL> [--backup-url URL]...` | 添加自定义电台 |
| `radio station edit <UUID> [--name NAME] [--url URL] [--backup-url URL]...` | 编辑自定义电台 |
| `radio station remove <UUID> [--keep-references]` | 删除自定义电台 |
| `radio station backups <UUID> [--add URL]... [--remove URL]...` | 管理备用地址 |

`PAGE` 表示 `[--limit N] [--offset N] [--all]`。列表命令的 limit 范围为 1–1000，
默认 100；`--all` 从指定 offset 起导出全部匹配项。

播放、录音与诊断：

| 命令 | 用途 |
| --- | --- |
| `radio play <UUID\|URL>` | 播放已保存电台，或试听 HTTP(S) 流 |
| `radio record <UUID> [--duration D] [--output-dir DIR]` | 录制已保存电台，直到停止或达到时长 |
| `radio recordings list [PAGE]` | 列出录音记录 |
| `radio recordings recover` | 重试中断录音的待收尾发布 |
| `radio recordings play <ID>` | 播放录音 |
| `radio recordings remove <ID> [--index-only]` | 删除录音及文件，或只删除索引记录 |
| `radio recordings open` | 打开录音目录 |
| `radio devices` | 列出 mpv 音频输出设备 |
| `radio doctor` | 检查媒体工具、录音路径和剩余空间 |

Shell 集成：

| 命令 | 用途 |
| --- | --- |
| `radio completions <SHELL>` | 为 bash、elvish、fish、powershell 或 zsh 生成补全 |
| `radio man` | 生成 `radio(1)` 手册页 |
| `radio help [COMMAND]` | 打印命令帮助 |

示例：

```sh
radio sync
radio search jazz --country FR --min-bitrate 128
radio search 新闻 --language chinese
radio random --country CN
radio play https://example.com/live.mp3
radio station add "我的电台" https://example.com/live \
  --backup-url https://example.com/backup
radio record <UUID> --duration 30m --output-dir ./recordings
radio --json-v2 search jazz
radio completions zsh > _radio
```

搜索默认在本地进行且不区分大小写。查询包含两个及以上字符时，也会按子串匹配
国家/地区中文名，例如 `刚果` 可命中两个刚果，`法国` 可命中法国。单字符查询只匹配
电台文本，因为单个汉字被过多地区共用。国家/地区、语言和标签筛选使用完整目录值；
语言也接受目录代码，例如 `zh`。`--min-bitrate` 会排除码率未知的电台。
`random` 从整个筛选结果中抽样，不受当前分页限制。

直接 URL 是临时试听：不会写入目录或历史，也不会替换上次保存的电台。
需要录音时请先用 `station add` 保存。

旧脚本可继续使用 `--json`；需要原子成功/错误响应和稳定错误码时使用 `--json-v2`。
详见 [JSON 输出契约](docs/json-contract.zh-CN.md)。

## 配置

配置文件是可选的。默认路径如下：

| 项目 | Linux | macOS |
| --- | --- | --- |
| 配置 | `${XDG_CONFIG_HOME:-~/.config}/radio-cli/config.toml` | `~/Library/Application Support/radio-cli/config.toml` |
| 数据目录 | `${XDG_DATA_HOME:-~/.local/share}/radio-cli/` | `~/Library/Application Support/radio-cli/` |
| 数据库 | `<数据目录>/radio.db` | `<数据目录>/radio.db` |
| 录音 | `<数据目录>/recordings/` | `<数据目录>/recordings/` |

使用 `--config <FILE>` 和 `--data-dir <DIR>` 可隔离或迁移状态。
`--api-url` 覆盖配置中的目录服务器。命令行选项优先于配置文件。
相对路径在启动时按工作目录解析。

| 类别 | 配置项 |
| --- | --- |
| 目录与网络 | `api_url`、`connect_timeout_secs`、`request_timeout_secs`、`auto_sync_days` |
| 播放 | `mpv`、`volume`、`normalize_loudness`、`asiafm_metadata_patch`、`audio_device`、`resume`、`playback_timeout_secs` |
| 录音 | `ffmpeg`、`ffprobe`、`recording_dir`、`probe_timeout_secs`、`recording_min_free_mb` |
| 界面 | `language`、`theme`、`artwork`、`mouse`、`keybindings.jump_to_playing` |

可以从 [config.example.toml](config.example.toml) 开始。各配置项的合法值、默认值、
路径优先级、语言检测、恢复模式、artwork 行为和快捷键语法见
[配置参考](docs/configuration.zh-CN.md)。

```toml
mpv = "mpv"
volume = 80
resume = "selection"
auto_sync_days = 7
theme = "default"
artwork = "station"

[keybindings]
jump_to_playing = "<F6>"
```

手写配置文件不会被改写。运行时音量和上次电台保存在 SQLite 中。
在 TUI 中用 `:set` 修改的设置只作用于当前会话。

## 录音

录音使用流拷贝，不重新编码，也不降低音质。开始前会探测实际流编码。

| 流编码 | 输出格式 |
| --- | --- |
| MP3 | `.mp3` |
| AAC，包括常见 HLS AAC | `.aac` |
| Vorbis | `.ogg` |
| Opus | `.opus` |

每个录音先写入唯一的 `.part` 文件，正常停止后发布为最终文件。JSON sidecar 记录电台、
实际请求地址、时间、编码、时长、完成状态和重连次数。最终文件不会覆盖已有文件。
中断的录音保留临时文件，可用 `radio recordings list` 查看；
`radio recordings recover` 可重试未完成的发布。

暂停播放不会暂停录音。切换电台会结束当前录音。`--duration` 接受秒、`s`、`m`、`h`，
范围为 1 秒至 365 天。格式、空间预检、重连和恢复步骤见
[录音指南](docs/recording.zh-CN.md)。

## 故障排查

| 现象 | 首先尝试 |
| --- | --- |
| 缺少 mpv、FFmpeg 或 ffprobe | 安装工具后运行 `radio doctor` |
| 目录为空或过旧 | 运行 `radio sync`，检查网络和代理设置 |
| 流播放失败或没有声音 | 用 mpv 直接试播 URL，检查音量和静音状态，再运行 `radio devices` |
| 另一个会话占用数据目录 | 关闭其他 TUI/播放/录音会话，或换用 `--data-dir` |
| 配置修改不生效 | 检查配置路径、TOML 语法和未知字段，然后重启 radio-cli |
| 录音不完整或无法发布 | 保留 `.part` 文件，查看 `radio recordings list`，再运行 `radio recordings recover` |
| artwork 不显示 | 运行 `radio sync`，确认终端支持对应图片协议，并检查详情面板尺寸 |

代理、播放设备、退出码、macOS Gatekeeper、终端和语言问题见
[故障排查指南](docs/troubleshooting.zh-CN.md)。

## 开发

```sh
cargo fmt --check
cargo clippy --all-targets --all-features --locked -- -D warnings
cargo test --all-targets --all-features --locked
cargo test --release --test storage catalog_benchmark -- --ignored --nocapture
```

测试使用本地 HTTP/ICY/HLS fixture，不依赖公共电台。录音测试需要 FFmpeg；
mpv 测试使用空音频输出，找不到 mpv 时跳过，可用
`RADIO_TEST_MPV=/absolute/path/to/mpv` 指定程序。发行打包、平台要求和
确定性目录基准见 [CONTRIBUTING.md](CONTRIBUTING.md)。

## 文档

| 文档 | 内容 |
| --- | --- |
| [安装指南](docs/installation.zh-CN.md) | 运行依赖、npm/crates/源码安装、发行包校验 |
| [配置参考](docs/configuration.zh-CN.md) | 全部配置项、路径、语言、恢复、artwork、快捷键 |
| [录音指南](docs/recording.zh-CN.md) | 格式、文件、恢复、重连、空间检查 |
| [故障排查](docs/troubleshooting.zh-CN.md) | 常见故障、代理、设备、退出码 |
| [JSON 输出契约](docs/json-contract.zh-CN.md) | JSON 结构、错误码与进程退出码 |
| [更新日志](CHANGELOG.md)（英文） | 版本说明与兼容性变化 |
| [贡献指南](CONTRIBUTING.md)（英文） | 开发、测试和发行归档 |
| [安全策略](SECURITY.md)（英文） | 支持版本和漏洞报告 |

`docs/` 中的配对指南均提供英文和简体中文版本。

## 许可证

MIT，见 [LICENSE](LICENSE)。随附的中文国家/地区名称表使用
[Unicode License V3](locales/LICENSE.unicode)。
