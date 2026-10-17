# radio-cli

Linux 优先的轻量 CLI / TUI 网络收音机。Rust 管理本地目录与交互，SQLite 保存收藏和历史，mpv 播放，FFmpeg 无损录音。无需账号或 API Key。

## 安装与启动

需要 Rust 1.88+、mpv、FFmpeg（含 ffprobe）。SQLite 已编入程序，不要求安装 SQLite 命令行工具。

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

直接运行 `radio` 会在交互终端打开 TUI；重定向输出时显示帮助。没有目录时，TUI 和 `random` 先获取热门 1,000 台；完整目录通过 `sync` 获取。同步失败保留之前的目录。

## 命令

```sh
radio search jazz --country FR --min-bitrate 128
radio search 新闻 --language chinese
radio search --tag 'public radio' --limit 100 --offset 100
radio --json search jazz
radio info <UUID>
radio play <UUID>
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

名称、国家、语言和标签支持本地搜索；国家代码、语言和标签筛选使用完整值（语言也可用目录提供的代码，例如 `zh`）。最低码率筛选排除码率未知的记录。`random` 在整个匹配目录中抽样，不受搜索分页限制。`info` 始终输出 JSON；查询命令可加 `--json`。运行状态与诊断写入 stderr。

电台采用完整 Radio Browser UUID；自定义电台创建本地 UUID。收藏、最近播放和自定义地址不被目录同步覆盖。收藏中已从目录消失的电台仍可尝试播放。历史保留最近 1,000 次成功起播记录，界面按电台聚合显示最近收听。

TUI 按键：

| 按键 | 操作 |
|---|---|
| 方向键 / `j`、`k` | 选择电台 |
| Enter | 播放选中电台 |
| Space | 暂停 / 恢复播放，录音继续 |
| `n` | 按当前筛选随机下一台 |
| `f` | 收藏 / 取消收藏选中电台 |
| `r` | 开始 / 停止当前电台录音 |
| `+`、`-` | 音量 |
| `/` | 搜索，Enter / Esc 结束输入 |
| `F` | 国家、语言、标签、最低码率筛选 |
| Tab | 电台 / 收藏 / 最近 / 录音 |
| PgUp / PgDn | 电台列表翻页 |
| `q` / Ctrl-C | 收尾录音并退出 |

## 配置与数据

Linux 默认路径：

- 配置：`${XDG_CONFIG_HOME:-~/.config}/radio-cli/config.toml`
- 数据：`${XDG_DATA_HOME:-~/.local/share}/radio-cli/radio.db`
- 录音：数据目录下的 `recordings/`

配置文件可选。参见 [config.example.toml](config.example.toml)，使用 `--config 文件` 指定；`--data-dir 目录` 可隔离测试数据。相对目录按启动时的工作目录解释；录音数据库保存绝对路径。

默认发现 Radio Browser 服务器并有限重试。可设置 `api_url`，或使用优先级更高的 `--api-url https://服务器`。HTTP 请求支持 `HTTP_PROXY`、`HTTPS_PROXY`、`ALL_PROXY`、`NO_PROXY` 等标准环境变量；音频后端遵循其各自的代理支持，目录可达不代表音频流可达。目录超过七天提示刷新，不会静默启动全量同步。

同一数据目录只允许一个播放 / 录音 / TUI 会话；其他 CLI 查询和收藏操作仍可使用。退出前台即停止音频，长时间运行可放在 tmux 或 systemd 中。

## 录音行为

- MP3 → `.mp3`；AAC（包括常见 HLS AAC）→ `.aac`；Vorbis → `.ogg`；Opus → `.opus`。根据实际音轨探测，不盲信目录的编码字段。
- 使用流拷贝，不重编码、不降低音质；不支持的录音编码明确报错。
- 每个录音有 JSON 元数据，包含电台、实际请求地址、时间、编码、媒体时长和完成状态。源站重定向后的最终 CDN 地址不一定可见。
- 先写唯一 `.part` 文件；正常停止后生成最终文件。中断 / 失败保留 `.part` 和状态，后续启动媒体会话时恢复未收尾记录。`recordings list` 可查看结果；媒体时长可能为估计值。
- TUI 切台先结束当前录音；新电台需重新按 `r`。暂停播放不会暂停录音。直播源提前结束不视作完整录音。
- `--duration` 接受秒、`s`、`m`、`h`，范围为 1 秒至 365 天。切点以音频包为单位，不能保证精确歌曲边界。
- 边听边录最多两条并发上游连接；编码探测连接先关闭，再启动录音连接。HLS 一个媒体任务可能请求多个分片。
- 文件只保存在本地；请遵守电台条款与适用的个人录制规定。

如需恢复 `.part`，先复制到新文件，再根据原编码用 FFmpeg 流拷贝。不要直接覆盖原文件。

## 开发与验证

```sh
cargo fmt --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --all-targets --locked
cargo test --release --test storage catalog_benchmark -- --ignored --nocapture
uv run scripts/measure_resources.py --binary target/release/radio
```

测试使用本地 HTTP/ICY/HLS 源，不依赖公共电台。录音测试需要 FFmpeg；mpv IPC 测试使用空音频输出，可通过 `RADIO_TEST_MPV=/绝对路径/mpv` 指定，未找到 mpv 时会跳过相关测试。CI 安装并执行真实 mpv 测试。资源目标与结果见 [验证记录](docs/validation.md)。

首版不提供后台服务、回溯缓存、歌曲识别、歌词、地图和后台全目录探测。目录来源为 [Radio Browser](https://www.radio-browser.info/)，音频直连电台；不使用 RoamDial 私有服务，也不会按同名自动合并备用线路。更详细的设计见 [实现说明](docs/implementation.md)。
