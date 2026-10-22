# 录音指南

[English](recording.md) | **简体中文**

radio-cli 使用 FFmpeg 以流拷贝方式录制选中的音频流，不重新编码、不转码，
也不会主动改变音质。录音既可在 TUI 中操作，也可通过独立 CLI 命令完成。

## 开始与停止

```sh
radio record <UUID>
radio record <UUID> --duration 30m --output-dir ./recordings
```

独立命令会一直运行，直到按 `Ctrl-C` 或达到指定时长。进度和输出路径写入 stderr。
在 TUI 中按 `r` 开始或停止录制当前播放电台。播放必须已进入播放或暂停状态；
若仍在连接，请等待起播。临时直接 URL 需先保存为自定义电台才能录音。

`--duration` 接受纯秒数以及带 `s`、`m`、`h` 的值，范围为 1 秒至 365 天。
时长只是停止期限，不保证源站会提供足量音频。切点以音频包为单位，
无法保证落在精确的歌曲或节目边界。

暂停播放不会暂停录音。TUI 中切换电台会结束当前录音；要录制新电台需再次按 `r`。

## 格式

radio-cli 在启动 FFmpeg 前用 ffprobe 探测实际音轨，不会盲信目录中的编码字段。

| 探测到的编码 | 容器 / 扩展名 |
| --- | --- |
| MP3 | MP3 / `.mp3` |
| AAC，包括常见 HLS AAC | ADTS AAC / `.aac` |
| Vorbis | Ogg Vorbis / `.ogg` |
| Opus | Ogg Opus / `.opus` |

不支持的编码会在开始录制前报错。即使启用了 `normalize_loudness`，录音仍是流拷贝；
该配置只影响播放。

## 文件与命名

每个录音先写入目标目录中唯一的 `.part` 文件。正常结束后，radio-cli 验证音轨、
发布最终文件并写入 JSON sidecar。最终文件不会覆盖已有文件。

默认目录是数据目录下的 `recordings/`。可用 `recording_dir` 或 `--output-dir` 覆盖；
相对路径在启动时按工作目录解析。

文件名包含电台名；如果录音是在播放会话中启动的，还会包含当时已知的曲名。
之后切歌不会重命名文件。未知曲名使用仅含电台名的格式。不安全字符会被移除，
曲名词语间的空白变为下划线，名称同时受字符数和字节数限制。

完成或失败的录音都会在媒体文件旁生成 sidecar，记录电台、radio-cli 实际请求的地址、
时间、编码、媒体时长、完成状态、重连次数以及可用时的错误诊断。
源站重定向后，最终 CDN 地址可能不可见。

## 状态与恢复

录音数据库记录 `recording`、`complete`、`interrupted`、`failed`、`finalizing` 等状态。
可用以下命令查看和管理：

```sh
radio recordings list
radio recordings play <ID>
radio recordings remove <ID>
radio recordings remove <ID> --index-only
radio recordings open
```

`recordings remove` 默认删除索引对应的媒体文件和 sidecar；`--index-only` 只删除索引。
`recordings open` 使用平台文件管理器打开配置的录音目录。

中断或失败的录音会保留 `.part` 文件和诊断状态。`radio recordings recover`
重试处于待收尾状态的录音发布，但不会把中断的直播采集变成完整录音。

macOS 上的发布要求文件系统支持硬链接。发布中断时请保留文件并先运行
`radio recordings recover`，不要提前手动清理。

如需手动恢复 `.part`，先复制成新文件，再根据原编码用 FFmpeg 重封装或流拷贝。
不要覆盖临时文件的唯一副本。

## 重连与不完整采集

HTTP/ICY 连接异常截断时使用 FFmpeg 的重连能力，指数退避上限为 2 秒；
HLS 分片重新拉取失败也计入重连。
重连可能留下直播音频缺口；完成的录音会保留重连次数并显示提示，
因为媒体中可能存在缺段。

重试耗尽后，radio-cli 保留临时文件和失败诊断。直播源提前结束不会被当作完整录音，
有限长度的源也不会循环播放来凑足指定时长。

边听边录最多使用两条并发上游音频连接。编码探测连接会在录音连接启动前关闭。
一个 HLS 媒体任务可能请求多个分片。曲目封面所用的 ICY 探测占用同一音频连接预算，
并会在开始录音前让出连接。artwork 图片下载和目录 HTTP 请求不计入该预算。

## 磁盘空间

录音前会检查目标文件系统。估算顺序优先使用探测到的音频流码率，其次容器码率，
再次目录码率，并加上 `recording_min_free_mb` 配置的最低余量。

无法得知时长或码率时只能检查最低余量，并提示总大小未知。预检无法保证长时间录音
不会在途中写满磁盘；空间紧张时请监控目标分区。
相关配置见[配置参考](configuration.zh-CN.md#录音配置)。

## 隐私与合规

录音只保存在本机。再分发录音前，请确认电台条款以及你所在地区适用于广播录制的规则。
