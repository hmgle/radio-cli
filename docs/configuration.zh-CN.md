# 配置参考

[English](configuration.md) | **简体中文**

配置文件是可选的；省略的配置项使用下列默认值。
[`config.example.toml`](../config.example.toml) 提供了带注释的起点。
命令行选项优先于配置文件。

## 路径与优先级

| 项目 | Linux 默认值 | macOS 默认值 |
| --- | --- | --- |
| 配置文件 | `${XDG_CONFIG_HOME:-~/.config}/radio-cli/config.toml` | `~/Library/Application Support/radio-cli/config.toml` |
| 数据目录 | `${XDG_DATA_HOME:-~/.local/share}/radio-cli/` | `~/Library/Application Support/radio-cli/` |
| 目录数据库 | `<数据目录>/radio.db` | `<数据目录>/radio.db` |
| 录音 | `<数据目录>/recordings/` | `<数据目录>/recordings/` |
| 命令历史 | `<数据目录>/command-history.json` | `<数据目录>/command-history.json` |
| artwork 缓存 | `<数据目录>/artwork/` | `<数据目录>/artwork/` |
| 运行时锁 | `<数据目录>/run/media.lock` | `<数据目录>/run/media.lock` |

解析规则：

- `--config FILE` 指定配置文件。显式指定的文件不存在会报错；默认配置文件不存在时使用默认值。
- `--data-dir DIR` 同时替换默认数据目录及其下的数据库位置。
- `--api-url URL` 覆盖 `api_url` 配置。
- 相对路径（包括 `recording_dir`）在启动时按工作目录解析；实际录音目录以绝对路径保存。
- 配置在命令启动时读取，程序不会改写配置文件。修改后需重新启动 radio-cli。

## 播放配置

| 配置项 | 默认值 | 合法值与行为 |
| --- | --- | --- |
| `mpv` | `"mpv"` | 播放、枚举设备和检查滤镜使用的程序名或路径。 |
| `volume` | `80` | 0 到 100 的整数。运行时音量保存在 SQLite 中，后续会话继续使用。 |
| `normalize_loudness` | `false` | 仅对播放启用响度均衡，录音仍为流拷贝。 |
| `asiafm_metadata_patch` | `false` | 可选的 AsiaFM 亚洲粤语台曲目标题整理，见 [AsiaFM 曲目信息](#asiafm-曲目信息)。 |
| `audio_device` | 自动选择 | 可选的 mpv 输出设备名；省略或设为 `""` 时自动选择。 |
| `resume` | `false` | `false`、`true` 或 `"selection"`，见[恢复模式](#恢复模式)。 |
| `playback_timeout_secs` | `12` | 1 到 300 的整数，播放起播期限。 |

修改配置中的 `volume` 只影响尚无运行时音量记录的会话。
已保存的运行音量越界时回退到配置值。

### 恢复模式

| 值 | TUI 启动时的行为 |
| --- | --- |
| `false` | 打开全部电台来源，不播放。 |
| `true` | 恢复上次来源和上次成功播放的电台，并开始播放。 |
| `"selection"` | 恢复上次来源并选中上次电台（包括所在分页），但不启动音频。 |

上次电台不可用时会跳过或按场景回退到首行。录音来源没有可恢复的电台选择。
音量独立记忆，并始终恢复。

### 响度均衡

`normalize_loudness = true` 会让 mpv 使用短窗口 `f=150:g=5` 的 `dynaudnorm`。
它只影响播放，会增加少量缓冲，不会修改录音。每个进程对配置的 mpv 程序只检查一次
滤镜支持；缺少滤镜时请关闭该选项或安装提供该滤镜的 mpv。

### AsiaFM 曲目信息

在配置文件顶层设置 `asiafm_metadata_patch = true`，可整理 AsiaFM 亚洲粤语台
（UUID `fb1a81c7-cdf2-4ed8-912c-796918d0fa08`）的受损曲目标题。
直接播放或自定义电台使用 `http://yyt.asiafm.net:8000/asiafm` 时也适用，
同一主机、端口和路径的 HTTPS 地址同样支持。修改配置后需重启 radio-cli。

补丁会移除标题末尾已知的 `(AsiaFM亚洲粤语台)` 尾注（包括受损形式），
合并多余空白，并将 `�?` 简化为表示缺失的 `�`。
例如，`张国荣  (AsiaFM亚洲粤语�?)` 显示为 `张国荣`，
`陈小�?- ? (AsiaFM亚洲粤语�?)` 显示为 `陈小� - ?`。
无论是否启用该选项，程序都会尝试解码完整的 GBK 文本；补丁不会猜补歌手或歌名。

末尾括号之外的裸台名会保留，因为它可能是节目名的一部分，所以水印可能仍有残留。
匹配到已知尾注后，标题内所有 `�-` 都会规范为 `� -`，包括原本有意如此书写的内容。

对于这个电台，整个字段只剩 `ç` 等 Latin-1 形式的 UTF-8 引导字节时，
补丁将其视为截断文本，显示为 `�`。该规则只覆盖三／四字节引导字节（`E0`–`F4`）；
二字节引导字节（`C2`–`DF`，如 `Ã`）与真实 Latin-1 字母重叠，因此保留。
这是针对该电台的假设：
该位置若恰好是真实的单个重音字母，也会显示为 `�`；`Café`、`Jóga` 等完整单词会保留。
其他电台、未知格式的尾注和本地文件标签不应用这些规则。

封面匹配使用同样整理后的标题。播放期间开始的新录音，会将该标题保存到文件名和
`track` 元数据中；既有录音不会重写。该选项不增加网络请求，也不能找回上游已丢弃的字节。

## 录音配置

| 配置项 | 默认值 | 合法值与行为 |
| --- | --- | --- |
| `ffmpeg` | `"ffmpeg"` | 录制和收尾媒体时使用的程序名或路径。 |
| `ffprobe` | `"ffprobe"` | 录制前检查流信息时使用的程序名或路径。 |
| `recording_dir` | `<数据目录>/recordings` | 输出目录；相对路径在启动时解析。 |
| `probe_timeout_secs` | `5` | 1 到 300 的整数，录制前独立的编码探测期限。 |
| `recording_min_free_mb` | `16` | 正整数 MiB 最低余量；可以估算总大小时会叠加上估算值。 |

空间预检是保护措施，不是空间预留。无法得知时长或码率时，只检查配置的最低余量，
并提示无法估算总大小。详见[录音指南](recording.zh-CN.md)。

## 目录与网络配置

| 配置项 | 默认值 | 合法值与行为 |
| --- | --- | --- |
| `api_url` | 自动发现 | 兼容 Radio Browser 的 HTTP(S) 服务地址；`--api-url` 优先。 |
| `connect_timeout_secs` | `5` | 1 到 300 的整数，目录、artwork 及相关 HTTP 请求的连接超时。 |
| `request_timeout_secs` | `20` | 1 到 300 的整数，目录和 artwork 请求的整体超时。 |
| `auto_sync_days` | `7` | 无符号 16 位整数（0–65535）；已同步目录达到该天数时后台刷新，`0` 表示关闭。 |

### 目录来源与同步

电台目录来自第三方服务 [Radio Browser](https://www.radio-browser.info/)，
缓存在本地 SQLite 数据库中。源码仓库和发行包都不附带目录数据库。

Radio Browser 服务器会自动发现，并在有限次数内故障转移。可设置 `api_url` 或使用
`--api-url` 指定兼容服务。完整 `radio sync` 会获取所有可用分页。本地没有目录时，
TUI 和 `radio random` 先获取最热门的 1,000 个电台。

如果一次本可成功的同步会让旧目录缩减超过 25%，radio-cli 会拒绝它，
除非使用 `radio sync --accept-shrink`。失败、取消或被拒绝的同步都会保留旧目录。
TUI 同步也可用 `:sync-cancel` 取消。

`auto_sync_days` 只刷新曾经完成过完整同步的目录。自动同步在 TUI 后台运行，
并遵循相同的取消和缩减保护。

目录中的电台成功起播后，会向 Radio Browser 上报一次点击；重连不会重复上报。
自定义电台和临时直接流不会上报点击。

### 代理

目录和 artwork 的 HTTP 请求支持标准 `HTTP_PROXY`、`HTTPS_PROXY`、`ALL_PROXY` 和
`NO_PROXY` 环境变量。mpv 与音频后端使用各自的代理配置，因此目录可达不代表音频流可达。

## 界面配置

| 配置项 | 默认值 | 合法值与行为 |
| --- | --- | --- |
| `language` | 自动检测 | `"en"` 或 `"zh-CN"`；未知值提示一次并回退英文。 |
| `theme` | `"default"` | `"default"`、`"mono"` 或 `"light"`；非空 `NO_COLOR` 强制 mono。 |
| `artwork` | `"off"` | `"off"`、`"station"` 或 `"track"`，见 [artwork](#artwork)。 |
| `mouse` | `true` | TUI 是否捕获鼠标；`false` 允许终端原生选择文本。 |
| `keybindings.jump_to_playing` | `"''"` | 定位当前播放电台的快捷键，见[快捷键](#快捷键)。 |

### 语言

语言按以下顺序取第一个非空值：

1. `--lang`
2. `RADIO_LANG`
3. 配置文件中的 `language`
4. `LC_ALL`
5. `LANG`
6. 英文

`en`、`C`、`POSIX` 和 `en-*` 选择英文；`zh`、`zh-CN` 和 `zh-Hans` 选择简体中文。
其他值会提示一次并回退英文。`radio --lang ...`、`RADIO_LANG` 和配置项只影响新进程；
`:set language en|zh-CN` 只改变当前 TUI 会话。

中文界面中的国家/地区名来自固定版本的 Unicode CLDR 48 名称表，
以上游 `countrycode` 为键。无法识别或缺失代码时，依次回退到上游目录名称、代码、未知。
存储和 JSON 保留上游 `country`、`countrycode` 原值，`--country` 仍使用上游代码。

### 音频设备

`audio_device` 选择 mpv 输出设备。运行 `radio devices` 可查看配置的 mpv 程序
提供的设备名和说明。名称取决于 mpv 音频后端和本机硬件，重新连接设备后可能变化；
空值表示自动选择。

配置的本地播放器或输出设备启动失败时，会结束本次播放请求，不会尝试备用电台或线路，
因为更换音频流无法修复本地媒体配置问题。

## Artwork

artwork 是可选的 TUI 详情图片，不影响音频播放或录音。

| 值 | 来源 | 回退 |
| --- | --- | --- |
| `"off"` | 不加载图片 | 纯文字详情 |
| `"station"` | 当前展示电台的目录 `favicon` | 纯文字详情 |
| `"track"` | 当前曲目的 ICY `StreamUrl` 图片 | 电台台标，再回退纯文字 |

台标来自目录元数据，不探测音频流。升级旧目录后运行 `radio sync`，
可为已有记录补齐 `favicon`。

只有当详情面板展示的电台正在播放，并且 ICY `StreamUrl` 提供可用图片地址、
当前曲目标题非空时，才会请求曲目封面。探测间隔为 25 秒，可能被源站计为额外听众；
暂停、停止、切台或退出会停止探测。HLS 和本地录音回放不参与探测。
录音期间，封面探测让位于编码探测和录音连接，详情回退到台标或文字。

终端支持 Kitty、Sixel 或 iTerm2 图片协议时使用对应协议，否则使用 Unicode 彩色半块。
tmux 固定使用半块。图片在详情文字下方居中，不会移动文字。单色主题、
小于 100×24 的终端，或文字下方空间不足时，详情保持纯文字。

仅下载不超过 256 KiB 的 JPEG/PNG，并限制解码尺寸。缓存位于数据目录的 `artwork/`，
按最近使用时间裁剪到最多 128 项、32 MiB。图片 URL 变化会生成新的缓存项；
超过 1 MiB 的缓存文件会在解码前丢弃。

部分 Sixel 终端（包括 xterm）的图片被弹窗或文字覆盖后可能消失，
关闭并重新打开详情通常可重绘。

## 快捷键

`[keybindings] jump_to_playing` 修改普通模式下定位当前播放电台并聚焦列表的快捷键。

支持的格式：

- 1 到 4 个可打印、非空白字符，例如 `"gp"`。
- 一个放在尖括号中的命名键，可带修饰符，例如 `"<F6>"`、`"<Ctrl-p>"`、
  `"<Alt-p>"` 或 `"<Shift-P>"`。
  命名键包括 `F1`–`F12`、`Space`、`Home`、`End`、`Enter`、`Tab` 和方向键，
  也可以是任意单个字符。`"<Shift-Tab>"` 按 BackTab 处理。
- `""` 禁用该快捷键。

默认值为 `"''"`：一秒内连按两次单引号。Ctrl-C 保留用于退出。
配置的绑定优先于已有的普通模式快捷键。目标在当前页时保留来源、筛选和排序；
否则切换到全部电台、清除筛选并加载目标分页。定位不会重新播放。

内置帮助面板会显示实际配置的按键；禁用时不显示该条目。

## 校验与数据归属

未知 TOML 字段会被拒绝，避免拼写错误被静默忽略。URL 非法时返回 `invalid_url`；
超时越界、最低余量为零、快捷键无效或音量大于 100 时，启动会以 `invalid_settings`
错误失败。

radio-cli 不会改写手写配置文件或 `config.example.toml`。可变状态位于所选数据目录：

- `radio.db` 保存目录、收藏、历史、录音索引、运行时音量、上次电台和同步元数据。
  历史保留最近 1,000 次成功播放。
- `command-history.json` 保存最多 100 条 TUI 命令；包含 URL 凭据的命令不会保存。
- `artwork/` 是可删除、大小有上限的图片缓存。
- `run/media.lock` 串行化播放、录音和 TUI 媒体会话。

同一数据目录同时只允许一个媒体会话。其他一次性查询和收藏操作仍可打开数据库。
并发运行多个手动会话时，请为每个会话使用不同的 `--data-dir`。
