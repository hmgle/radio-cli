# 播客

[English](podcasts.md) | **简体中文**

radio-cli 支持公开 RSS 2.0、Atom 的音频附件，独立保存播客订阅、历史、进度和下载。
小宇宙节目有公开 RSS 时即可订阅；分享链接不保证能发现 RSS。不支持账号同步、
私人订阅同步或付费内容解锁。

## TUI 日常操作

在来源中选择播客，打开节目，再按 Enter 播放单集。收听时按 **p**（或输入
`:podcast` / `:subtitles`）打开播客操作菜单。菜单显示当前单集名称，可选择显示字幕、
本地生成字幕、导入字幕、下载音频、加入队列和从头播放。使用 `j/k`、Enter、显示的
数字键或鼠标选择，Esc 关闭。无需查找、复制单集 ID。

**p 操作当前播放的单集，暂停时也有效**，不受列表选中行影响。如需操作其他单集，
在播客单集、下载或队列列表中选中后按 **P**。没有播客播放时，p 也可打开选中单集的
操作。菜单或导入提示打开后，目标保持固定，不随队列自动切集改变。
选择**显示字幕**时，如果已切换到其他单集，会续播菜单中显示的单集；
如果仍在播放该单集，则保留播放位置与暂停状态。

常用命令默认作用于当前播放单集：

```text
:subtitles on
:subtitles transcribe
:subtitles import "单集字幕.vtt"
:podcast download
:podcast queue add
:podcast play --from-start
:podcast mark played
```

`:podcast subtitles`、`:podcast queue remove`、`:podcast mark unplayed` 以及
`:podcast downloads pause|resume|cancel|remove` 也支持省略 ID。
没有正在播放或暂停的播客时会提示如何选择，不会猜测历史记录，也不会操作直播电台。
通过菜单导入时，路径绑定菜单中显示的单集；Esc 取消输入。含空格的文件路径请加引号。
下载列表显示单集标题。

## 订阅和播放

```sh
radio podcast discover https://example.org/show
radio podcast preview https://example.org/feed.xml
radio podcast add https://example.org/feed.xml
radio podcast list
radio podcast episodes FEED_ID
radio podcast play EPISODE_ID
radio podcast play EPISODE_ID --from-start --speed 1.25
radio podcast refresh
radio podcast unsubscribe FEED_ID
```

`discover` 识别网页声明的 RSS/Atom，也能转换 Apple 节目链接；多结果时手动选择源。
`preview` 不保存订阅，`add` 明确订阅但不播放。节目和单集列表支持 `--limit`（默认
100，范围 1–1000）、`--offset`、`--all`。使用
`radio podcast episodes FEED_ID --query "标题"` 或
`radio podcast list --query "节目"` 可先按本地标题过滤，再分页。
TUI 的 `/` 搜索同样覆盖当前节目的全部单集。

`radio podcast search 查询词 [--country CN] [--limit 25]` 搜索 Apple Podcasts，
地区指目录地区而非节目语言。缺少 `feedUrl` 的结果不能订阅。目录不可用不妨碍已有
本地节目和下载播放。订阅后的封面与媒体来自 RSS，不使用 Apple 的试听音频。

自然播放结束标记已听；中断后保存位置，已听完的单集再次播放从头开始。进度每五秒
提交一次，暂停、切换、停止和正常退出也会提交；保存失败会提示，异常退出可能丢失
最新更新。不能定位的源保持暂停并说明原因，可选择从头播放或先下载。

`radio podcast mark EPISODE_ID played|unplayed` 修改已听状态；标记未听同时清零位置。
刷新不删除源中暂时缺失的旧集。源声明的长度或时长改变时清除完成标记，保留位置。

## TUI

来源增加播客、播客下载和播客队列。Enter 打开节目，再按 Enter 播放单集；Esc 返回。
搜索和发现结果按 Enter 预览，再按 Enter 确认订阅。`:podcast add URL` 也先预览。

不知道搜什么时，在播客来源按 **D** 或执行 `:podcast browse` 打开发现页，可选热门
播客、19 个大类、“随便看看”、搜索、切换地区或回到订阅。p 菜单在播放时也提供
发现入口。分类列表无需联网，Enter 进入对应地区／分类的 Apple Podcasts 榜单。
榜单内 **R** 随机抽取同一地区／分类的候选；Enter 预览，Esc 逐层返回原结果与光标。
已能精确匹配到本地订阅的条目显示“已订阅”，Enter 直接打开单集。

```text
:podcast browse
:podcast categories --country CN
:podcast charts --country CN
:podcast charts --country CN --category technology
:podcast charts --country CN --category 1318 --random --limit 10
```

这些命令也支持 CLI（去掉冒号、添加 `radio` 前缀）。分类接受 `categories` 列出的
ID 或英文名称。地区优先使用命令参数，其次为配置 `podcasts.country`，默认中文 CN、
英文 US。榜单显示作者、类别和原排名；“随便看看”从当前地区／分类最多 100 个热门
候选中抽取，不是全网随机或个性化推荐。榜单在本次进程中缓存 10 分钟；F5 重新加载
当前目录，随机页会重新抽取。目录内 Page Up/Down 移动光标，不会切换到订阅列表。
网络或限流错误可用 `?` 查看，Esc 返回仍使用本次会话保存的页面。

```text
:podcast search 知行小酒馆
:podcast refresh
:podcast download
:podcast downloads list
:podcast queue add
:podcast queue play
:podcast seek 12:30
:podcast speed 1.5
:podcast auto-next on
:sync-cancel
```

Space 暂停/继续。Alt-Left/Right 默认后退/前进各 15 秒；精确定位接受秒数或 `HH:MM:SS`，
依赖 mpv 的定位能力。倍速范围 0.5–3.0，独立于直播保存。`/` 在本地搜索节目/单集标题，
F5 重新加载视图；`:podcast refresh [FEED_ID]` 抓取 RSS。`:sync-cancel` 也取消当前
播客命令并停止视图的定时重载，F5 可重新加载。下载需用对应 pause/cancel 命令。
播客列表中 f 切换稍后收听，F 输入播客筛选条件；实时录音操作不适用于播客。

现有 `resume` 配置也用于播客：`selection` 恢复最后单集及其所在页的选择但不播放，
`playback` 续播最后单集，
`disabled` 不自动恢复播放。

## 下载与队列

```sh
radio podcast download EPISODE_ID
radio podcast downloads list
radio podcast downloads pause EPISODE_ID
radio podcast downloads resume EPISODE_ID
radio podcast downloads cancel EPISODE_ID
radio podcast downloads remove EPISODE_ID
radio podcast queue add EPISODE_ID
radio podcast queue list
radio podcast queue move EPISODE_ID 1
radio podcast queue remove EPISODE_ID
radio podcast queue clear
radio podcast queue play --auto-next
```

CLI 在前台下载，TUI 最多并发两个。退出后保留任务，下次 TUI 启动恢复；暂停、取消、
失败的任务需手动继续。仅在资源校验信息和 Range 响应有效时续传，否则重新下载。
文件以 `.part` 写入，通过 ffprobe 检查后发布。附件 URL、声明大小和时长与当前单集
一致时，播放优先使用完成的下载；文件丢失或媒体已变更时回退在线附件。旧版本下载
记录没有媒体指纹，也需要重新下载后才能用于本地播放。显式下载会替换过期文件，
过期的部分下载会从头开始。保留至少 200 MiB 空间；节目自动清理默认关闭，
开启后独立调度，不与刷新或取消订阅绑定。

队列手动加入、去重并持久保存。播放完成消费当前项，默认不连播；TUI 使用
`:podcast auto-next on` 开启，CLI 使用 `--auto-next`。错误或手动停止不推进队列；
直接播放单集不消费队列，刷新不会自动入队新集。

## 同步字幕

在 TUI 中执行 `:subtitles on`，会加载 RSS `podcast:transcript` 标签链接的时间轴
字幕。支持 UTF-8 编码的 WebVTT 和 SRT；没有时间轴的 HTML/纯文本文稿无法同步。
已有订阅需先刷新一次，才能发现字幕链接。字幕跟随 mpv 的音频位置，支持续听、暂停、
跳转和倍速。读取已有字幕不需要语音模型。CLI 播放仍只输出音频。

针对当前播客，按 p 选择**生成字幕（边播边显示）**即可：检查本地运行时和模型后，
按 15 秒窗口读取音频，每识别出一句就更新详情，无需等整集下载或识别完成。
已有完整且匹配的下载会复用，否则直接读取音频链接中的片段。生成必须主动触发；
`:subtitles on` 只显示已有或源提供的字幕，不会自动开始大量下载或识别。
请先准备运行时和模型，见[字幕设置](configuration.zh-CN.md#直播字幕)。

```text
:subtitles transcribe
:subtitles import "单集字幕.vtt"
:subtitles fetch
:subtitles cancel
```

`transcribe` 优先识别当前播放位置，再向后提前生成。跳转后优先处理新位置，当前
正在识别的窗口会先完成。模型在窗口间保持加载，已完成的窗口（包括静音）单独保存，
取消或退出后再次生成可以继续；识别到结尾后补齐前面的缺口。只有整集覆盖完成才
替换完整字幕缓存，此前的源字幕或导入字幕仍保存在磁盘上。

首次字幕仍需等待模型加载和识别。CPU 较慢、倍速较高或服务器不支持高效定位时，
字幕可能暂时落后；播放不会因此暂停。远程片段读取失败时，请取消字幕任务、
下载音频后重试。TUI 不再自动下载整集；独立 CLI 的 `--transcribe` 仍保留下载完整
音频后生成整集字幕的流程。

`import` 复制本地 UTF-8 VTT/SRT 文件，之后不再依赖原文件。`fetch` 使用缓存或
获取源中支持的字幕。识别时间戳对应语音段，不提供逐字对齐；准确率取决于模型和音频。

字幕任务独立于列表浏览，切换播客页面不会中断，期间仍可操作播放。
`:subtitles cancel` 仅取消字幕任务；`:sync-cancel` 取消字幕及普通播客命令。
完成提示显示处理的单集名称，若该单集仍在播放则自动重载时间轴；旧单集任务不会
覆盖当前其他单集的字幕。

字幕在**详情**面板中自动换行，当前句段加粗显示。窄终端按 `3` 打开详情。
详情获得焦点后，`v` 切换字幕与普通信息；`j/k`、方向键及 Page Up/Down 手动滚动，
并停止跟随；`End` 恢复跟随当前句段。手动滚动后，面板和底栏会标明“手动阅读”；
按 `3` 再按 `End`，或执行 `:subtitles follow`，可恢复自动滚动。
Alt+左/右仅改变播放时间，保留现有的跟随或手动阅读模式。补齐较早字幕会保留手动阅读位置；
切换单集或开始新的播放会恢复跟随。`:subtitles panel` 切换视图，
`:subtitles follow` 打开字幕并恢复跟随。底部字幕条仅保留一行状态。
直播字幕也使用该面板，跟随最新识别内容；其采集时间戳仍独立于播放器进度。

`:subtitles history` 打开带音频时间戳的完整字幕时间轴。`:subtitles clear`
仅清空显示，不删除缓存；`:subtitles retry` 重新加载。缓存位于
`<数据目录>/podcasts/transcripts`，不随音频下载目录配置改变。分段识别以
`.partial.json` 单独记录覆盖范围；继续生成要求音频、模型、语言及分段设置均匹配。
缓存绑定音频附件 URL、声明大小和时长，源提供的字幕还绑定其字幕 URL。
这些信息变化后缓存失效。若服务端在不改变这些信息的情况下替换音频或插入广告，
无法可靠检测，需要导入匹配字幕或对下载音频重新转写。
字幕文件上限为 10 MiB、100,000 个句段。不提供翻译，播放不等待识别或字幕请求。

## OPML 与配置

```sh
radio podcast import subscriptions.opml --dry-run
radio podcast import subscriptions.opml
radio podcast export subscriptions.opml
```

先完整验证 OPML，再逐源导入并报告结果；重复源去重，部分失败不删除成功项。
取消时立即终止导入并返回取消错误，保留此前成功导入的订阅。
导出仅包含订阅信息，不含进度或下载；覆盖已有导出文件须使用 `--overwrite`。

```toml
[podcasts]
# directory = "/path/to/podcasts"
refresh_hours = 6
```

下载默认在 `<data-dir>/podcasts`，相对目录按启动工作目录解析。TUI 运行期间默认
每六小时刷新到期源，`refresh_hours = 0` 关闭自动刷新；失败退避最多一天。
不安装后台系统服务。抓取限制为五次重定向、解压后 10 MiB、64 层和每源 10,000 集，
超限或错误保留旧内容。仅支持 HTTP(S)，拒绝 XML DTD。日期、时长和长度均可缺失，
播放时长优先以 mpv 实测为准。脚本使用 `--json-v2`，见 [JSON 合同](json-contract.zh-CN.md)。

## 脚本中的显式 ID

独立 CLI 进程没有当前 TUI 播放上下文，因此单集命令仍需显式 ID；TUI 也保留显式
指定其他单集的能力：

```sh
radio podcast episodes FEED_ID --json-v2
radio podcast subtitles EPISODE_ID --transcribe
radio podcast subtitles EPISODE_ID --file episode.vtt
```

脚本可读取 `podcast episodes` 返回的 `items[].id`；节目 ID 来自 `podcast list`
或 `podcast add`。日常交互优先使用上面的菜单和当前单集命令。

## 返回当前单集与收听库

连续按两次 `'`（沿用 `jump_to_playing` 自定义绑定），或执行 `:podcast current`，
回到当前播客所属节目，跳到单集所在分页并选中它。播放和暂停均适用，不重新播放。
窄终端在定位后显示列表，按 Esc 可恢复原来的焦点、搜索和分页。
定位和返回会保留字幕／信息视图，以及字幕跟随状态或手动阅读位置。
信息视图的滚动位置会随导航重置。节目内 Esc 也会恢复订阅列表
原来的搜索和分页。在线搜索结果和预览页在本次会话内保存，返回时无需重新联网，
并恢复光标与滚动位置。列表中的播放／暂停标记与光标选择分别显示；回到节目列表
仍可看到当前单集所属节目的状态标记，连接中及播放失败也有对应提示。

按 p 打开当前播放菜单；P 操作选中的单集。菜单支持滚动，前九项可用数字选择，
其他项目用 j/k、Enter 或鼠标操作。没有播客播放时，在播客来源按 p 可管理选中节目，
或打开搜索目录、添加 RSS、OPML 导入导出和收听库入口。
取消订阅、清空队列及删除音频会要求确认；取消订阅保留本地收听数据与音频。

```text
:podcast library all
:podcast library inbox
:podcast library continue
:podcast library history
:podcast library saved
:podcast library all --feed FEED_ID --status unplayed --sort oldest
:podcast library all --downloaded --query 标题
:podcast save
:podcast save --selected
:podcast save EPISODE_ID --remove
:podcast dismiss EPISODE_ID
```

收件箱只收录订阅后刷新发现的新单集，首次订阅不导入整个历史库。
`dismiss` 将单集移出收件箱，不修改收听进度。最近收听按单集去重。
`--status` 接受 `all|unplayed|progress|played`，`--sort` 接受 `newest|oldest|title`；
搜索、筛选和排序在分页前完成。

TUI 省略单集 ID 的命令继续作用于当前播放；`--selected` 显式改为列表选中项。
在列表按 x 勾选单集（可跨页），X 批量标记已听，U 批量标记未听；确认中列出固定目标，
标记未听会清零进度。脚本也可使用 `radio podcast mark-batch ID1 ID2 [--unplayed]`。
节目可以使用精确标题代替 ID，单集可以使用带引号的 `节目标题::单集标题`；重名时提示
候选 ID，不会随机选择。`radio podcast resume` 续播最后的播客，不回退电台。

## 队列、章节、书签与定时

队列中 Enter 从选中项进入队列播放模式，前面的项目保留；默认仍不连播。
菜单中可开启连播、下一集播放、移出队列、上移和下移。自然结束消费当前队列项；
暂停、停止和错误不推进。Alt-Left/Right 使用相对定位，连续按键可以累计。

```text
:podcast queue next EPISODE_ID
:podcast queue start EPISODE_ID
:podcast auto-next on
:podcast chapters
:podcast bookmarks
:podcast bookmark --note "想再听一次"
:podcast bookmark EPISODE_ID --time 12:30 --note "重点"
:podcast bookmark-edit BOOKMARK_ID --note "新备注"
:podcast bookmark-edit BOOKMARK_ID
:podcast sleep 30
:podcast sleep +15
:podcast sleep end
:podcast sleep off
```

章节支持 Podcasting 2.0 JSON 和 Podlove Simple Chapters；远程章节失败时可回退
源中内嵌章节。章节和书签列表按 Enter 跳到对应时间，同一单集暂停时跳转保留暂停。
书签独立保存，删除下载或标记未听不会删除书签。

睡眠定时按实际经过时间计时，暂停时继续倒计时；到期暂停并保存位置。
`end` 在本集结束时阻止续播；切换媒体、停止或退出会取消定时，不在重启后恢复。

## 节目设置与下载策略

通过节目菜单进入设置，或使用 `radio podcast settings FEED_ID [KEY VALUE]`。
只提供节目 ID 时查看全部设置；设置 `inherit` 可恢复可继承字段的默认值。

| 设置 | 默认与含义 |
|---|---|
| `speed` | 继承全局播客倍速；0.5–3.0 |
| `sort` | `newest`；可用 `oldest`、`title` |
| `skip_back` / `skip_forward` | 各 15 秒 |
| `skip_intro` / `skip_outro` | 0 秒，关闭自动跳过 |
| `completion_seconds` | 0，关闭接近结尾自动标记 |
| `auto_download` | `false`，主动开启后仅下载以后发现的新集 |
| `auto_cleanup` | `false`，主动开启后清理已听下载 |

```text
:podcast settings FEED_ID speed 1.25
:podcast settings FEED_ID sort oldest
:podcast settings FEED_ID skip_forward 30
:podcast settings FEED_ID completion_seconds 30
:podcast settings FEED_ID auto_download true
:podcast downloads stats
```

设置在后续播放或进入节目时生效，当前播放倍速可直接用菜单或 `:podcast speed` 调整。
显式播放倍速优先于节目设置，再继承全局值。片头跳过不覆盖续播位置；明确“从头播放”
从零开始。片尾跳过要求时长可靠，按自然完成处理。
可选的接近结尾标记要求实际播放进入尾段；单纯跳转不会立即触发，也不会提前推进队列。

自动下载不回填历史、不自动入队；自动清理只删除工具管理的音频，保留进度和书签，
播放、字幕任务或队列使用的文件延后清理。自动化只在客户端运行期间执行。
下载按入队顺序调度，最多两个并发。临时网络错误最多额外重试三次，退避 5、30、120 秒，
并遵守更长的服务器重试时间；暂停、取消、磁盘不足及无效媒体需手动处理。
列表显示可靠时的百分比与传输速度，总量未知时只显示字节数。

刷新和 OPML 导入最多四个网络请求并发；普通浏览不会取消这些任务。
单个节目失败不阻止其他节目刷新，失败原因可在列表、详情与任务区查看。
临时重定向不永久改写订阅地址，迁移冲突保留原订阅并报告，不自动合并收听记录。
目录限流期间立即提示，已有订阅、下载和本地浏览继续可用。
无 `--json` 的 CLI 输出可读文本；终端下载进度写 stderr，机器输出合同保持独立。
目录地区优先 `--country`、再取 `[podcasts].country`，否则中文 CN、英文 US。
