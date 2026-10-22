# JSON 输出与进程退出码

[English](json-contract.md) | **简体中文**

需要统一的机器可读响应时，使用全局 `--json-v2`。成功时 stdout 恰好包含一个 JSON 值：
`{"ok":true,"data":...}`。没有结果的命令返回 `null`。失败时为
`{"ok":false,"error":{"code":"...","message":"...","params":{...}}}`。
进度写入 stderr。错误码和参数名在英文、中文下保持稳定；消息文本跟随所选语言。
未归类的 I/O 错误使用 `general_error`。不要解析消息文本。

`--json-v2` 要求显式指定非交互命令，且与 `--json` 冲突。
显式 `--help` 和 `--version` 仍输出普通文本。
带 `--json-v2` 时，用法错误（包括未知命令）也会返回错误封套。
失败命令不会先输出成功封套。

| 退出码 | 含义 |
| --- | --- |
| 0 | 成功，包括帮助和版本输出 |
| 1 | 操作、配置、网络、存储或媒体失败 |
| 2 | 命令行用法错误 |
| 3 | 必需的可执行程序无法启动 |
| 4 | 另一个媒体会话占用了运行时锁 |

旧版 `--json` 仍可使用。其成功结构如下：

| 命令 | 旧版数据结构（也是 v2 的 `data`） |
| --- | --- |
| `search` | 电台数组；保留给既有脚本 |
| `info`、`station edit` | 电台对象；两者始终输出 JSON |
| `station add --json` | 电台对象 |
| `favorites list`、`history`、`station list`、`recordings list` | 分页对象：`items`、`total`、`offset`、`limit`、`has_more` |
| `countries`、`languages`、`tags` | `{value,count}` 数组 |
| `favorites add/remove` | `{stationuuid,name,favorite}` |
| `station remove` | `{stationuuid,removed,placeholder_kept}` |
| `station backups` | URL 数组 |
| `sync` | `{stations,last_sync}` |
| `record` | 包含最近一条录音的数组 |
| `recordings recover` | `{warnings}` |
| `recordings remove` | `{id,removed,index_only}` |
| `devices` | 配置的 mpv 返回的 `{name,description}` 数组 |
| `doctor` | 包含 `checks`、`config`、`database` 的对象 |
| `play`、`random`、`recordings play/open` | 无旧版 JSON 结果；v2 的 `data:null` |

使用 `--all` 时，分页对象的 `limit` 为 `null`；否则为请求的页大小。
`offset` 从 0 开始。JSON 保留用户字符串（含 Unicode）；
只有面向人的终端输出会清理控制字符。旧版失败会在 stderr 输出诊断，并使用相同退出码。
旧版 `doctor`、恢复和录音命令可能在非零退出前先输出诊断数据。
需要原子成功/错误响应时优先使用 v2。

示例：

```sh
radio --json search jazz                  # 旧版数组
radio --json-v2 search jazz               # {"ok":true,"data":[...]}
radio --lang en --json-v2 info missing    # station_not_found；退出码 1
```

`completions <shell>` 和 `man` 正常返回生成的文本；带 `--json` 时返回 JSON 字符串，
带 `--json-v2` 时字符串位于封套的 `data` 中。生成命令只为选择语言而读取配置，
不会创建数据目录或打开 SQLite。

录音对象包含 `reconnects`，即 FFmpeg 报告的 HTTP 重试次数。次数大于零表示即使
`status` 为 `complete`，直播音频也可能存在缺口；缺少该字段的旧元数据按 0 处理。

电台对象包含 `favicon`，即可选的目录台标 URL。旧存储记录和自定义电台默认使用空字符串。
运行 `sync` 可为已有目录电台补齐该字段。

`info`、`station edit/remove/backups` 和 `recordings remove` 即使没有 JSON 参数也会输出 JSON。
对于电台删除，`removed:true` 表示自定义电台被删除；`placeholder_kept:true`
明确表示仍保留一个仅含名称的失效记录，用于引用或 `--keep-references`。
`--json-v2=true` 这类非法等号形式属于用法错误，也会收到 v2 错误封套（该选项是无值开关）。

录音对象可能包含 `track`，即播放中请求录音时捕获的曲名。未知时省略该字段，
包括旧记录和独立 CLI 录音；之后切歌不会改变它。

配置的音频输出失败返回 `audio_device_failed`，并带 `device` 参数（退出码 1）。
本地播放器启动错误保留原始错误码，例如 `missing_dependency`（退出码 3）、
`player_startup`（退出码 1）或 `normalization_unavailable`（退出码 1）。
这些失败会结束本次播放请求，不尝试音频流回退或刷新目录；正常收尾后错误码仍会保留。
