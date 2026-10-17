# 首版验证记录

验证日期：2026-10-17。环境：Debian Linux x86_64，内核 6.12.85，Intel Core i5-8365U（8 个逻辑 CPU），Rust 1.97.1、FFmpeg 7.1.3、mpv 0.40.0。release 构建开启 thin LTO，二进制约 7.51 MiB。

## 功能与工程检查

- `cargo fmt --check`、`cargo clippy --all-targets --locked -- -D warnings`。
- 20 项功能测试通过；另显式运行并通过五万条目录的性能测试。
- 自动测试覆盖目录分页与原子回滚、无效上游条目、中文与字面搜索、组合筛选、收藏保留、全目录随机抽样、CLI JSON 和配置错误。
- 实际 FFmpeg 验证 MP3、AAC、Vorbis、Opus 流拷贝及 HLS AAC；验证文件编码、时长、JSON 元数据、失败文件保留、提前 EOF 和中断恢复。磁盘写入错误使用失败后端模拟，未向真实文件系统灌满数据。
- 实际 mpv 使用 `--ao=null` 验证 ICY 标题、播放、暂停、音量、显式备用地址及起播超时。验证前台 SIGKILL 后子进程停止。
- 伪终端验证 TUI 渲染、调整窗口、`q`、Ctrl-C、SIGTERM，以及终端模式恢复。未进行物理扬声器听音验证。
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
| TUI 空闲 | radio | 12.29 | 0.39% |
| 播放 | radio | 12.41 | 0.39% |
| 播放 | mpv | 65.18 | 2.56% |
| 播放并录音 | radio | 12.71 | 0.40% |
| 播放并录音 | mpv | 65.41 | 2.39% |
| 播放并录音 | ffmpeg | 50.26 | 0.60% |

应用本体达到 RSS ≤30 MiB、空闲 CPU <1% 的目标；播放并录音的整套 RSS 约 **128.4 MiB**。资源脚本不计其自身和本地测试服务器。五秒采样受系统负载与 CPU 时间分辨率影响；实际音频设备、TLS、HLS 和电台编码会改变结果，不能据此承诺所有音源的总占用。

复现需要 mpv、FFmpeg 和 uv：

```sh
cargo build --release --locked
uv run scripts/measure_resources.py --binary target/release/radio
```
