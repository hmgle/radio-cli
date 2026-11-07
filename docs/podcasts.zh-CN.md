# 播客

[English](podcasts.md) | **简体中文**

radio-cli 支持公开 RSS 2.0、Atom 的音频附件，独立保存播客订阅、历史、进度和下载。
小宇宙节目有公开 RSS 时即可订阅；分享链接不保证能发现 RSS。不支持账号同步、
私人订阅同步或付费内容解锁。

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

```text
:podcast search 知行小酒馆
:podcast refresh
:podcast download EPISODE_ID
:podcast downloads list
:podcast queue add EPISODE_ID
:podcast queue play
:podcast seek 12:30
:podcast speed 1.5
:podcast auto-next on
:sync-cancel
```

Space 暂停/继续。Alt-Left/Right 后退/前进 15 秒；精确定位接受秒数或 `HH:MM:SS`，
依赖 mpv 的定位能力。倍速范围 0.5–3.0，独立于直播保存。`/` 在本地搜索节目/单集标题，
F5 重新加载视图；`:podcast refresh [FEED_ID]` 抓取 RSS。`:sync-cancel` 也取消当前
播客命令；下载需用对应 pause/cancel 命令。播客不适用电台收藏和实时录音操作。

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
文件以 `.part` 写入，通过 ffprobe 检查后发布。播放优先使用完成的下载，文件丢失时
回退在线附件。保留至少 200 MiB 空间，不自动删除文件，取消订阅也保留下载和进度。

队列手动加入、去重并持久保存。播放完成消费当前项，默认不连播；TUI 使用
`:podcast auto-next on` 开启，CLI 使用 `--auto-next`。错误或手动停止不推进队列；
直接播放单集不消费队列，刷新不会自动入队新集。

## OPML 与配置

```sh
radio podcast import subscriptions.opml --dry-run
radio podcast import subscriptions.opml
radio podcast export subscriptions.opml
```

先完整验证 OPML，再逐源导入并报告结果；重复源去重，部分失败不删除成功项。
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
