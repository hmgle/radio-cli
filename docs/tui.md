# TUI 操作与设计

运行 `radio tui`，按 `:` 输入内置命令，Enter 执行，Esc 回到普通模式。比如首次打开后输入 `:sync`，即可在界面内同步完整目录；期间仍能切换面板、搜索、暂停和调音量。任务面板显示进度、完成状态和错误，`:sync-cancel` 请求取消；取消时保留原目录；正在进行的 HTTP 请求在后台结束或超时，其结果被忽略，不阻塞媒体控制和退出。

`F` 打开筛选弹窗，从本地目录后台加载国家、语言、标签候选和电台数；↑/↓ 选择并填入，
Tab 切换字段，Enter 应用，Ctrl-U 清空全部筛选，Esc 取消；点击底栏筛选摘要也可清空。也可直接输入。`:search --sort bitrate --min-bitrate 128`
按码率排序；可选排序为 `name|bitrate|votes|clickcount|random`，默认 `clickcount`。

## 面板与焦点

| 编号 | 面板 | 操作 |
|---|---|---|
| `1` | 来源 | `j/k` 选择电台、收藏、最近、录音；Enter 进入列表 |
| `2` | 列表 | `j/k` 选择、Enter 播放、`f` 收藏；`gg/G` 首尾，Ctrl-u/d 移动十项 |
| `3` | 详情 | `j/k` 或 PgUp/PgDn 滚动电台与录音信息 |
| `4` | 播放器 | Enter 打开当前电台完整详情 |
| `5` | 任务与消息 | `j/k` 滚动历史消息，默认跟随最新消息 |

Tab / Shift-Tab 循环切换面板；Ctrl-h/j/k/l 或 Ctrl-方向键按位置切换。左右方向键与 `h/l` 也移动焦点。`[` / `]` 在任意面板切换来源分类。鼠标点击聚焦面板、选中列表项，滚轮在所在面板导航。

活动面板用亮青色粗边框、`▶` 标记和高亮标题表示，列表选中项另有反色与箭头；底栏同时显示 `NORMAL` 和面板名。终端禁用颜色时仍可通过粗边框与箭头识别。切换输入模式时，底栏显示 `COMMAND` 或 `SEARCH`，普通模式的播放、录音、退出按键不再响应文本输入。

100 列且至少 20 行时显示三列主区；窄终端采用上下布局。较矮终端的列表与详情共享区域，用 `2` / `3` 切换。最小尺寸为 40×16；更小时显示调整提示，`q` 可退出。

## 命令

| 命令 | 含义 |
|---|---|
| `:sync` / `:sync-cancel` | 后台同步 / 请求取消目录任务 |
| `:search jazz --country FR --min-bitrate 128` | 按 CLI 参数搜索当前电台分类 |
| `:play [UUID]` | 播放列表选中项或指定电台 |
| `:random [搜索参数]` | 随机电台；无参数时沿用当前筛选 |
| `:info [UUID]` | 在详情面板查看选中或指定电台 |
| `:current` | 查看当前播放电台完整详情 |
| `:favorites list` | 切换到收藏列表 |
| `:favorites add [UUID]` / `:favorites remove [UUID]` | 收藏 / 取消收藏，省略 UUID 时使用选中项 |
| `:history` / `:recordings list` | 最近播放 / 录音列表 |
| `:station add '我的电台' https://example.com/live --backup-url https://example.com/backup` | 添加自定义电台和备用地址 |
| `:station list` / `:station edit UUID --name 名称 --url URL` | 列出或修改自定义电台；`--backup-url` 可替换备用地址 |
| `:station remove UUID [--keep-references]` | 按 `y` 二次确认，`n`/Esc 取消；被引用时保留名称占位并清除播放地址 |
| `:station backups UUID [--add URL] [--remove URL]` | 查看或修改备用地址，详情面板也展示备用地址 |
| `:record [UUID] --duration 30m --output-dir './my music'` | 录音；省略 UUID 时录制当前实际播放线路 |
| `:record stop` | 收尾录音文件 |
| `:pause` / `:stop` / `:volume 70` | 暂停或恢复 / 停止播放并收尾录音 / 音量 |
| `:doctor` | 在任务面板展示依赖检查结果 |
| `:focus sources\|list\|details\|player\|activity` | 聚焦指定面板，也接受 `1`–`5` |
| `:help` / `:quit` / `:q` | 帮助 / 收尾并退出 |

命令只调用程序内置操作，不执行 shell。支持单引号、双引号和转义，路径相对启动目录解释。Tab / Shift-Tab 循环补全命令或参数名，↑/↓ 浏览本次会话历史，←/→、Home/End 编辑，Ctrl-a/e 到行首/尾，Ctrl-u 清空，Ctrl-w 删除前一个词。参数错误保留输入并展示原因；运行错误写入任务面板。进程级 `--config`、`--data-dir`、`--api-url`、`--json` 不可在已运行的 TUI 内修改。

`/` 进入增量搜索，200 ms 消抖；Enter 确认，Esc 恢复进入前的查询。`F` 编辑国家、语言、标签和最低码率。搜索期间方向键编辑输入，普通模式的 PgUp/PgDn 才切换数据库分页。`?` 打开可滚动帮助。

## 当前播放信息

顶部始终展示当前播放电台名称、编码、码率、音量、暂停与录音计时；高度足够时再显示国家、语言和实时曲名。列表移动不改变此处的播放对象。

按 `4` 再 Enter，或执行 `:current`，打开该电台的国家、语言、地区、码率、编码、HLS、标签、收藏状态、目录状态、UUID、流地址、实际播放请求地址与官网。录音列表的详情展示编码、容器、时长、文件大小、路径和失败原因。详情在列表移动或分类切换后恢复跟随选中项。

码率与编码来自 Radio Browser 目录，标注“目录”，可能与当前音源不同；自定义电台通常显示未知。这里尚未做实时流码率测量。实际录音编码仍通过 ffprobe 探测；播放曲名来自播放器收到的元数据。请求地址可反映备用线路，HTTP 重定向后的最终 CDN 地址未必可见。

## 设计参考与边界

面板编号、Tab 焦点、分类切换和上下文提示参考 [lazygit 的按键设计](https://github.com/jesseduffield/lazygit/blob/master/docs/keybindings/Keybindings_en.md)；视图导航、搜索与命令入口参考 [tig 手册](https://jonas.github.io/tig/doc/manual.html)，输入模式采用 Vim 风格。保留收音机所需的五个面板与快捷操作，避免引入完整 Vim 编辑器或 shell。

目录任务独立运行，重复同步会提示已有任务，失败与取消保留原目录。媒体命令使用同一有界队列；起播失败的网络回退和 ffprobe 录音探测仍可能暂时延迟后续媒体操作，界面可继续输入。没有后台守护进程；退出后停止媒体并等待任务收尾。

Ctrl-C 在所有 TUI 模式下收尾并退出；Esc 取消编辑或关闭弹窗。终端过小时，q 直接退出。

收藏、历史和录音列表均支持 PgUp/PgDn 分页，列表标题显示当前条数及总数。
命令行 `favorites list`、`history`、`recordings list` 支持 `--limit`、`--offset` 和 `--all`。
这些列表的 `--json` 输出为 `{items,total,limit,offset,has_more}`；`--all` 导出全部剩余条目，
此时 `limit` 为 null。`search --json` 为兼容现有脚本仍返回数组。录音按开始时间和 ID
稳定排序，更新录音状态不会将旧任务排到首位。TUI 使用分页，`--all` 仅供命令行导出。

F5 或 `:refresh` 刷新当前列表。媒体工作线程同时检测本连接写入与 SQLite
`data_version` 变化，播放成功后的历史、其他 CLI 会话的收藏编辑会自动反映到列表。
详情查询按请求编号展示加载、成功或错误状态，查询失败不会保留上一台的详情。

TUI 录音附属于当前播放会话：`:record UUID` 仅接受正在播放的同一电台，其他 UUID
明确报错；`r` / `:record` 录制当前实际线路（包括备份）。暂停不停止录音，换台、
播放失败、`:stop` 或退出会收尾录音；`:record stop` 仅结束录音。独立录制其他电台
请使用 CLI `radio record UUID`。播放器的 REC 标记对应当前播放电台。

本地录音列表中，Enter 播放选中的文件，`o`（或 `:open recordings`）
用 `xdg-open` 打开录音目录，`e` 在详情面板展开完整 FFmpeg 错误尾部。
`D` 删除选中录音，按 `y` 确认，`n`/Esc 取消。
默认先删除音频和 sidecar，成功后删除索引；文件操作失败时保留索引，
重试允许文件已不存在。`:recordings remove ID --index-only` 仅删除索引、
保留文件，同样需要确认。录制、收尾和仍待恢复的记录不能删除。
本地播放不产生电台收听历史，不发送目录点击统计，播放结束后自动停止。

CLI 对应命令为 `radio recordings play ID`、`radio recordings remove ID
[--index-only]`、`radio recordings open`。CLI 删除命令直接执行。
录音前检查可用空间：`recording_min_free_mb` 默认保留 16 MiB，再加上
已知码率乘以指定时长的估算空间。不足时拒绝开始，不创建临时录音。
`radio doctor --json` 的 `recording_space` 检查返回可用和最低空间字节数。
