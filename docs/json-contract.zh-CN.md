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
只有面向人的终端输出会清理控制字符。
流元数据会先解码，并不原样保留输入。启用
[`asiafm_metadata_patch`](configuration.zh-CN.md#asiafm-曲目信息) 后，
匹配电台的标题会先整理再使用；播放期间开始的新录音，其 JSON `track` 和 `path` 字段
会体现整理后的标题。
旧版失败会在 stderr 输出诊断，并使用相同退出码。
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


字幕管理命令始终输出 JSON。`subtitles setup` 返回 `{runtime,ready}`；
`subtitles doctor` 仅在检查全部通过后返回 `{runtime,model,ready}`；
`subtitles models list` 返回 `{id,bytes,license,license_url,present}` 数组。
`bytes` 和 `present` 包含运行该模型所需的所有文件，包括共用的 Silero VAD。
`present` 在共享资源锁内检查文件类型与大小，`models verify ID` 才执行 SHA-256 校验。
安装、校验、删除均返回 `{id,removed}`。V2 使用正常响应信封，进度输出至 stderr。
字幕错误码为 `subtitle_settings`、`subtitle_model`（参数 `id`）、
`subtitle_integrity`（`id`、`path`）、`subtitle_runtime`、
`subtitle_platform`（`platform`）、`subtitle_space`（`available`、`required`，
以字节为单位且包含预留空间）、`subtitle_busy`、
`subtitle_failed`（`detail`），退出码均为 1。此版本不提供 CLI 转写数据流。

Station 对象新增可空的 `geo_lat`、`geo_long` 经纬度字段。旧数据缺失时为 `null`；运行 `sync` 可补齐上游提供的坐标。地图仅使用完整、有效的坐标对，不推测位置。


## 播客结果

`podcast browse` 和 `podcast categories` 返回 `{items,total,feed_title}`；分类行包含
稳定的 `id`、`slug`，以及本地化 `title`、`kind` 和可供 TUI 执行的 `command`。
`podcast charts` 返回 `{items,total,country,category,random,source,feed_title}`；
`category` 为分类 ID 或 `null`。榜单行有 `collectionId`（字符串）、`collectionName`、
`artistName`、`collectionViewUrl`、`primaryGenreName`、`description` 和 `chartRank`。
`chartRank` 为随机抽取前的原排名。`total` 是本次返回的候选数，无额外分页。
匹配到已有订阅的搜索／榜单行可附带 `local_feed_id`；不得仅凭同名节目推断。
预览额外返回 `subscription_url`，保留用户选择的目录地址供确认订阅时使用。

`podcast list`、`podcast episodes` 沿用分页对象。ID 为本地 UUID。
节目包含 `id`、`url`、`title`、`author`、`description`、`image`、`website`、
`subscribed`，可空的 `etag`、`modified`、`refreshed_at`、`error`，以及刷新用的
`revision`、`failures`。单集包含 `id`、`feed_id`，可空的 `guid`、`published`，
`title`、`description`、`website`、附件 `url`/`mime`，可空的 `bytes`、`duration_ms`，
`position_ms`、可空的 `observed_duration_ms` 和 `completed`。位置和时长单位为毫秒；
`bytes` 是源声明值。

`preview` 返回未保存 ID 的 `{feed,episodes}`；`add` 返回节目对象；`discover` 返回
URL 数组；`search` 返回 Apple 目录对象数组（可能缺少 `feedUrl`）。`refresh` 返回
`{id,ok,error?}` 数组，部分失败在数组中呈现。`unsubscribe` 返回
`{id,subscribed:false}`；`mark` 返回单集对象。

`download` 和 `downloads` 修改命令返回下载对象，`downloads list` 返回数组。
字段为 `episode_id`、`state`、`bytes`，可空的 `etag`/`modified`/`error`，本地 `path`、
资源 `url` 与任务 `generation`。可空的 `media` 保存原始媒体快照
`{url,bytes,duration_ms}`，其中大小和时长允许为 `null`；本地播放和续传会将该快照
与当前单集核对。状态为 `queued`、`downloading`、`paused`、
`complete`、`cancelled`、`failed`、`removed`。队列列表和修改返回有序单集数组。
`play`、`queue play` 在播放退出后返回 v2 `data:null`。

OPML `import --dry-run` 返回 `{urls:[...]}`，导入返回逐源 `{id?,ok,error?}` 数组，
导出返回 `{exported:N}`。`speed` 返回 `{speed:N}`，`auto-next` 返回
`{auto_next:BOOL}`，定位操作需在 TUI 会话执行。播客操作失败使用 `podcast_failed`，
参数为 `detail`，退出码为 1；取消保留 `cancelled`。不要解析错误消息文本。

单集对象还包含 `transcripts` 数组，元素为源声明的 `{url,mime,language}`；
`language` 可为 `null`，旧单集默认空数组。
`podcast subtitles ID [--file FILE | --transcribe]` 返回 `{id,segments}`，
`segments` 为缓存的时间轴句段数。进度写入 stderr。字幕无效使用
`podcast_failed`；本地运行时和模型错误保留字幕错误码，取消保留 `cancelled`。
字幕命令不输出转写流。

下载列表对象额外包含可为 `null` 的 `title`，用于显示；身份仍由 `episode_id`
确定。`--transcribe` 先检查本地语音运行时/模型，再按需下载音频并生成
字幕。TUI 改为按片段读取并逐句显示，CLI 仍只输出一次最终结果。
CLI 仍需显式 ID；仅 TUI 命令可使用当前播放上下文。

`podcast library` 返回与 `episodes` 相同的分页结构；行额外包含 `author`、
`feed_title`、`queued`、`saved` 和可选的 `download_state`。`queue list` 也包含
这些单集展示字段。`download_state: "complete"` 表示本地文件存在且与当前附件匹配，
`"stale"` 表示已经完成的下载需要重新下载。下载对象的 `state` 仍记录传输结果。
节目列表增加
`unplayed_count` 和配置的 `sort`。节目对象增加可空 `new_feed_url`；单集增加
可空 `source_order`、`chapters_url` 和 `chapters:[{title,position_ms}]`。
旧数据的章节默认为空数组，可选字段为 null。刷新结果可额外包含 `title`。
下载增加可空 `total_bytes`、`retry_at`、`enqueued_at`，以及整数
`bytes_per_second`、`retries`（已经安排的额外重试次数，0–3）。下载列表同时携带
单集展示字段；未知总量保持 null。`downloads stats` 返回
`{downloads,bytes,completed,available_bytes}`，目录不可用时可用空间为 null。

`save` 返回 `{id,saved}`，`dismiss` 返回 `{id,dismissed}`，`mark-batch` 返回
`{marked}`。`settings` 返回[播客文档](podcasts.zh-CN.md#节目设置与下载策略)中的
完整节目设置对象。`chapters` 返回 `{items,total,episode_id}`，每项为
`{id,title,kind:"chapter",position_ms}`；`bookmarks` 返回 `{items,total}`，每项为
`{id,bookmark_id,title,position_ms,kind:"bookmark"}`，其中 `id` 是单集 ID，
`title` 为备注。`bookmark` 返回 `{id,episode_id,position_ms,note}`，其中 `id`
为书签 UUID；`bookmark-edit` 返回 `{id,removed}`。`resume` 和 `queue start`
沿用播放命令的结果合同。`current`、`sleep` 和相对定位是 TUI 交互操作；独立 CLI
没有控制另一个 TUI 会话的远程通道。脚本应继续使用稳定 ID；精确节目标题和
`节目::单集` 标题提供交互便利，重名会报错并给出候选 ID。
