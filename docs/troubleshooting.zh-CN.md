# 故障排查

[English](troubleshooting.md) | **简体中文**

先运行环境检查和内置诊断：

```sh
radio --version
radio doctor
radio --json doctor
```

`radio doctor` 检查配置的 mpv、FFmpeg、ffprobe 程序、录音目录可写性及剩余空间。
JSON 形式还会报告实际配置文件和数据库路径。它不测试电台流、音频输出或一般互联网连通性。

## 缺少媒体工具

通过包管理器安装 mpv 和 FFmpeg（含 ffprobe），然后重新运行 `radio doctor`。
若工具位于非标准路径，在配置文件中把 `mpv`、`ffmpeg` 或 `ffprobe` 设为可执行文件路径。
缺少必需程序时进程以状态码 3 退出。

```toml
mpv = "/opt/homebrew/bin/mpv"
ffmpeg = "/opt/homebrew/bin/ffmpeg"
ffprobe = "/opt/homebrew/bin/ffprobe"
```

## 目录为空或过旧

本地没有目录时，TUI 和 `radio random` 会先获取最热门的 1,000 个电台。
运行 `radio sync` 获取完整目录：

```sh
radio sync
radio countries --prefix CN
```

同步失败或取消会保留旧目录。若同步成功但目录缩减超过 25%，radio-cli 会把结果视为可疑，
要求用 `radio sync --accept-shrink` 明确确认。只有确实预期上游目录大幅缩减时才使用该选项。

在 TUI 中运行 `:sync`；`:sync-cancel` 可停止正在进行的同步，不替换当前目录。

## 同步或 artwork 无法访问网络

目录和 artwork 请求支持标准 `HTTP_PROXY`、`HTTPS_PROXY`、`ALL_PROXY` 和
`NO_PROXY` 环境变量。如果其他 HTTP 客户端可用而 `radio sync` 不可用，请检查这些变量。

默认 Radio Browser 服务发现不可用时，可显式选择兼容服务器：

```sh
radio --api-url https://de1.api.radio-browser.info sync
```

或在配置文件中设置 `api_url`。音频不经过目录 HTTP 客户端：mpv 直接连接电台，
并使用自己的代理设置。因此 `radio sync` 成功并不代表音频流可达。

## 播放失败或没有声音

先检查静音、暂停和音量，再列出 mpv 提供的设备：

```sh
radio devices
```

把 `audio_device` 设为其中一个名称，或删除该配置以自动选择。
硬件重新连接后，设备名可能变化。

直接用 mpv 测试电台 URL。如果 mpv 能播放而 radio-cli 不能，请检查 `radio doctor`、
配置的 `mpv` 路径以及是否启用了 `normalize_loudness`。缺少响度滤镜会有单独提示；
请关闭该选项或安装支持 `dynaudnorm` 的 mpv。

本地播放器故障不会通过切换电台修复。缺少可执行程序、播放器启动超时、
响度滤镜不可用或配置的音频设备失败都会结束本次请求，不会尝试备用地址、随机电台
或刷新目录。解决本地问题后重新发起播放。

网络流失败时，radio-cli 会按情况尝试备用地址、有限次直播重连和目录刷新。
即使最终恢复，重连也可能留下音频缺口。有限长度或反复抖动的源在重试耗尽后仍可能失败。

## 另一个媒体会话占用了数据目录

同一数据目录同时只允许一个播放、录音或 TUI 媒体会话，避免多个进程争用运行时锁和录音状态。
该故障以状态码 4 退出。

关闭另一个会话，或用不同数据目录隔离并发手动会话：

```sh
radio --data-dir /tmp/radio-second tui
```

媒体会话运行时，一次性目录查询和收藏操作仍可打开数据库。

## 配置修改不生效

检查实际路径：

```sh
radio --json doctor
```

然后确认：

- `--config` 指向正确文件；显式指定的文件必须存在。
- 文件是合法 TOML。
- 所有字段都受支持。radio-cli 会拒绝未知字段，避免拼写错误被静默忽略。
- 取值在文档规定的范围内。

配置只在进程启动时读取，程序不会改写配置文件。编辑后请重启 radio-cli。
TUI 中的 `:set language` 或 `:set mouse` 只作用于当前会话。
运行时音量和上次电台保存在 SQLite，不在配置文件中。

全部配置项和约束见[配置参考](configuration.zh-CN.md)。

## 录音失败或不完整

确认 FFmpeg、ffprobe 可用，目标文件系统可写且有足够空间：

```sh
radio doctor
radio recordings list
```

radio-cli 只以流拷贝方式录制 MP3、AAC、Vorbis 和 Opus。不支持的编码会在开始前失败。
直播源提前结束、重连耗尽、磁盘写满或音轨无效都可能使录音处于 `interrupted` 或
`failed`，同时保留 `.part` 文件和错误诊断。

收尾待重试时，请保留所有文件并运行：

```sh
radio recordings recover
```

macOS 上的发布要求文件系统支持硬链接。恢复前不要删除 `.part` 文件。
如需手动恢复，先复制文件，再按原编码做流拷贝或重封装。

## artwork 不显示或消失

运行 `radio sync`，让已有目录记录获得 `favicon` 元数据。台标是可选的，
自定义电台可能没有台标。曲目封面还要求当前标题非空，且 ICY `StreamUrl` 提供可用图片地址。

单色主题、小于 100×24 的终端，或详情文字下方空间不足时不会显示图片。
tmux 固定使用 Unicode 半块。部分 Sixel 终端（包括 xterm）的图片被弹窗或文字覆盖后
可能消失；关闭并重新打开详情可重绘。

缓存和协议细节见 [artwork](configuration.zh-CN.md#artwork)。

## 文本选择、颜色或终端布局异常

设置 `mouse = false` 或运行 `:set mouse false`，停止捕获鼠标事件，改用终端原生文本选择。
非空 `NO_COLOR` 环境变量会强制 mono 主题。浅色终端背景对比度不佳时，
可使用 `theme = "light"`。

TUI 会适配窄终端和矮终端。空间很小时部分面板会收起，详情会移到共享区域。
内置 `?` 帮助会列出当前上下文可用的按键；若控件或标签放不下，请增大终端窗口。

## macOS 阻止运行下载的程序

macOS 归档未经 Developer ID 签名或公证。先用 `SHA256SUMS` 校验归档，解压后，
仅在信任该发行版时去掉程序的 quarantine 属性：

```sh
xattr -d com.apple.quarantine ./radio
```

另见 [Apple 的说明](https://support.apple.com/en-us/102445)。也可使用 Rust 1.88
或更新版本从源码构建。

## 语言不是预期值

语言优先级为 `--lang`、`RADIO_LANG`、配置 `language`、`LC_ALL`、`LANG`，最后为英文。
不支持的值会提示并回退英文。可用 `--lang en`、`--lang zh-CN` 或
`radio --lang zh-CN tui` 测试指定语言。TUI 中的 `:set language en|zh-CN`
只改变当前会话。

中文界面的国家/地区名使用固定版本 CLDR 表。上游代码缺失或无法识别时，
回退到目录名称或代码；存储和 JSON 保留上游原值。

## 脚本输出不符合预期

运行状态和诊断写入 stderr。`radio info` 始终输出 JSON。
需要旧格式时用 `--json`；需要恰好一个成功或错误封套时用 `--json-v2`：

```sh
radio --json-v2 search jazz
radio --json-v2 info <UUID>
```

不要解析本地化消息文本，应使用 JSON 封套中的稳定错误码。
详见 [JSON 输出契约](json-contract.zh-CN.md)。

| 退出码 | 含义 |
| --- | --- |
| 0 | 成功，包括帮助和版本输出 |
| 1 | 操作、配置、网络、存储或媒体失败 |
| 2 | 命令行用法错误 |
| 3 | 必需的可执行程序无法启动 |
| 4 | 另一个媒体会话占用了运行时锁 |

## 仍未解决

请收集 `radio --version`、操作系统信息、`radio doctor` 输出、实际命令或 TUI 按键以及相关
stderr。提交 issue 前请隐去流地址中的凭据或私有 token。安全问题请按
[SECURITY.md](../SECURITY.md) 私下报告，不要提交公开 issue。
