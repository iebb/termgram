# Lua 配置

[English](../en/Configuration.md) · [指南首页](Home.md)

## 加载配置

把 `config.lua` 放在 `settings.conf` 旁，或用 `TERMGRAM_CONFIG` 指定路径。
使用 `:config reload`（或 `:reload`）即可加载修改，无需重启，可以从[配置示例](../../../examples/config.lua)开始。配置返回一个 Lua table，
支持 table、字符串、数学和 UTF-8 辅助函数，不提供文件系统、进程或插件接口。
启动时配置错误会显示提示并使用默认值；运行中重载失败则保留已经生效的设置。

```lua
return {
  chats = { work = -1001234567890 },
  ghost_text = "{send} 发送 · {newline} 换行",
  nerd_font = false,
  keymap = {
    { context = "conversation", on = { "<C-u>" }, run = "message_up", count = 20 },
    { context = "conversation", on = { "g", "w" }, run = "jump work" },
    { context = "compose", on = { "<Enter>" }, run = "newline" },
    { context = "compose", on = { "<C-s>" }, run = "send" },
  },
}
```

上下文包括 `global`、`chats`、`conversation`、`compose`、`edit`、`forward`、`poll`、`reactions`、`stickers`、`attachments`、`command`、`input`（登录、聊天过滤和帮助搜索）
、`help`、`overlay`、`preview`、`pins` 和 `search`。具体上下文优先于全局绑定。同一上下文中配置相同按键会替换默认绑定；
`run = "noop"` 删除绑定。组合键用独立按键列表表示，例如 `{ "g", "w" }`。
配置会检查前缀冲突。组合键一秒后过期，Escape 可以取消尚未完成的组合键。
按键表示法沿用 Yazi，例如 `<C-s>`、`<A-x>`、`<S-Enter>`、`<Tab>`、`<Esc>`。

聊天别名使用稳定的 Telegram 数字 ID，超级群为 `-100…` 格式；目标需要已经出现在
本地聊天列表中。绑定可选填 `desc` 作为帮助页说明。帮助页读取当前生效的绑定，
使用方向键滚动。

默认 `j/k` 在聊天之间或实际消息之间移动，`20k` 向上移动 20 条消息，必要时继续
加载更早的历史。`G` 或 End 回到最新消息；`gg` 或 Home 到当前已加载窗口的最早消息。
方向键按显示行滚动，PageUp/PageDown 每次滚动十行。`i` 输入，`R` 回复，`r` 跳转到
回复目标，`/` 在聊天列表中过滤，Tab 切换面板，`s` 设置，`a` 账号，`?` 帮助。
输入框内的普通文字不会触发导航组合键或数字前缀。

给 `message_up` 配置 `count = 20`，即可用一个快捷键向上跳 20 条。`count` 默认 1，
范围 1–9999，支持 `up`、`down`、`message_up`、`message_down`、`page_up`、`page_down`。
输入的数字前缀会乘以配置值，最终限制在 9999；其他动作不接受数量参数。

输入框占位提示中的 `{send}`、`{newline}`、`{cancel}`、`{stickers}` 会替换为实际快捷键。
`ghost_text = ""` 隐藏提示。应用内偏好单独保存，不会改写 Lua 文件。

按 `g i` 显示当前聊天 ID，可用来配置别名。

终端使用 Nerd Font Mono（v3+）时，可设置 `nerd_font = true`，启用聊天、文件夹、
归档、置顶和附件图标；默认 `false`。字体选择和回退方法见[外观](Appearance.md)。

## 底部状态栏

单行底栏替代原来的应用顶栏。可以配置左右组件及顺序；省略的字段保留以下默认值：

```lua
statusline = {
  enabled = true,
  left = { "mode", "app", "account", "message", "context" },
  right = { "notifications", "connection", "latency", "dc", "position" },
},
```

| 组件 | 含义 |
| --- | --- |
| `mode` | CHATS、NORMAL、SELECT、INSERT 或当前浮层模式 |
| `app` | Termgram |
| `account` | 本地账号槽位与 Telegram 显示名称 |
| `message` | 选中或最后可见消息的时间、送达、置顶和媒体状态 |
| `context` | 当前选择/帮助的实际快捷键，或可用更新 |
| `notifications` | 当前焦点聊天已确认的静音状态和本地截止时间（[说明](Notifications.md)） |
| `connection` | 正在连接、在线、重连或离线 |
| `latency` | 主连接最近一次成功 Ping 的毫秒数 |
| `dc` | 已认证会话的主数据中心 ID |
| `position` | 最新位置、距底部的显示行数，或阅读历史时的新消息数 |

每个组件在左右两侧合计只能出现一次，未知或重复组件会导致配置报错。空列表可以清空
一侧。窄窗口先隐藏次要组件，优先保留模式与当前操作；每侧的剩余组件维持原有顺序。
`enabled = false` 隐藏底栏，错误仍会在上方可读地显示。

延迟是可选诊断：在现有 Telegram 连接上每 60 秒最多发起一次 Ping，超时为 5 秒。
五秒未完成即显示不可用；旧探测结束前不追加探测，避免断网时积累请求。
结果包含 SDK 排队与重试时间，**不是消息到达延迟**。绘制过程中不发网络请求。
从两侧移除 `latency` 或关闭底栏，会停止这些额外请求。破折号表示尚不可用；断线会使
测量失效，超过 90 秒的样本不再显示，切换账号清空观测。DC 是会话的主数据中心，
不代表全部媒体传输服务器，也不根据地理位置猜测。

## 侧栏

```lua
sidebar = { width = 30, time_color = "cyan", unread_color = "yellow" },
```

宽度接受 24–60 个终端列，必要时收窄，给会话保留至少 48 列。低于 80 列时一次显示
一个面板。颜色使用[终端命名调色板](Appearance.md)。标题保留聊天单独配置的颜色；
时间与未读数各自占固定列，超过 999 显示 `999+`，不会撑宽列。

F4 在导航和输入时切换侧栏。宽窗口保留输入状态；窄窗口返回聊天列表并保留草稿。
Tab/Shift-Tab 或返回 Chats 也会展开侧栏。终端支持重复事件时，按住 F4 不会反复切换。
这里采用单次按键切换，不依赖按键释放事件。可以重新绑定：

```lua
{ context = "global", on = { "<A-b>" }, run = "toggle_sidebar" },
```

切换不会丢失会话、选择和消息。尚未打开聊天时保留列表；可见性是本地 UI 状态，
不会改变 Telegram 文件夹。

## 文件路径

`config.lua`、`settings.conf`、`appearance.json`、`navigation.json` 位于配置目录；
`navigation.json` 按 Telegram 账号 ID 记录上次聊天，不保存草稿文字。会话、消息库和媒体位于
数据目录。默认值如下：

| 系统 | 配置目录 | 数据目录 |
| --- | --- | --- |
| Linux | `$XDG_CONFIG_HOME/termgram` 或 `~/.config/termgram` | `$XDG_DATA_HOME/termgram` 或 `~/.local/share/termgram` |
| macOS | `~/Library/Application Support/dev.termgram.Termgram` | 相同 |
| Windows | `%APPDATA%/termgram/Termgram/config` | `%LOCALAPPDATA%/termgram/Termgram/data` |

账号 1 使用 `termgram.session`，其他槽位使用它旁边的
`accounts/<session filename>.account-N`，各自追加 `.cache.sqlite3` 数据库与 `.media`
目录。会话文件包含账号凭据，消息缓存与媒体是本地明文。清理方法见[缓存与同步](Synchronization.md)。

| 环境变量 | 用途 |
| --- | --- |
| `TELEGRAM_API_ID`、`TELEGRAM_API_HASH` | 源码构建的应用凭据；环境或 `.env` 优先于内置值 |
| `TERMGRAM_SESSION` | 指定账号 1 会话路径，其他槽位据此派生 |
| `TERMGRAM_CONFIG` | 指定 Lua 文件，在启动前的 shell 中设置 |
| `TERMGRAM_TMUX_PASSTHROUGH=1` | 启用终端透传设置，见[终端](Terminal.md) |

Lua 在凭据 `.env` 之前加载，因此 `TERMGRAM_CONFIG` 必须已存在于进程环境。
旧的 `TUIGRAM_SESSION` 和已有 TUIGram 默认会话仍被识别，以保留登录状态。
更改会话路径不会移动设置或颜色文件。

Lua 源码限制 64 KiB、VM 内存 8 MiB、约一百万条指令。未知字段、动作、别名和同上下文
前缀冲突会报错；启动时整个无效配置回退为默认值，`:config reload` 失败则保留当前配置。
不提供任意插件 API。

## 动作表

把动作放在合适的上下文。自定义 `global` 绑定作为后备，具体上下文的绑定或组合键前缀
优先。`desc` 自定义帮助文字；`noop` 删除当前上下文的绑定后，已有全局绑定可能重新生效。

| 动作 | 常用上下文与行为 |
| --- | --- |
| `quit`、`redraw`、`next_account`、`add_account` | 全局退出、重绘、账号控制 |
| `help`、`settings`、`accounts` | 导航；打开或切换浮层 |
| `reload_config` | 校验并应用 Lua 文件，无需重启 |
| `open`、`cancel`、`focus` | 按上下文激活、关闭、切换面板或二维码 |
| `toggle_sidebar` | 在导航或输入时展开/收起侧栏 |
| `up`、`down`、`page_up`、`page_down` | 列表选择、显示行滚动或搜索选择，支持 `count` |
| `message_up`、`message_down` | 会话消息光标，支持 `count` |
| `oldest`、`latest` | 首末聊天，或已加载历史起点/最新会话 |
| `first_unread`、`mark_read`、`mark_unread` | 跳到首条未读、明确将聊天全部标为已读、设置 Telegram 未读提醒 |
| `mentions` | 浏览 Telegram 未读提及及回复给你的消息 |
| `mute_chat`、`unmute_chat` | 设置当前聊天的 Telegram 通知静音 |
| `compose`、`send`、`newline` | 进入草稿或回复会话中明确选中的消息 / 发送 / 换行 |
| `edit_message`、`discard_edit`、`delete_message` | 编辑已送达消息、丢弃本地编辑或检查删除范围 |
| `copy_text`、`copy_link`、`forward_message`、`save_message`、`saved_messages` | 复制、预览转发或打开 Saved Messages |
| `attach`、`attachments`、`paste_clipboard` | 添加文件、查看附件或粘贴到草稿 |
| `remove_attachment`、`attachment_format` | 在附件列表中移除文件或切换照片/原文件格式 |
| `poll`、`toggle_poll_answer`、`retract_vote` | 打开投票、选择答案或准备撤回 |
| `reactions`、`clear_reactions` | 打开 emoji 回应或在选择器中移除本人的选择 |
| `stickers`、`sticker_set_next`、`sticker_set_previous` | 打开贴纸面板或切换其分区 |
| `spoilers`、`expand_quote` | 在会话中揭示/隐藏剧透、展开/收起引用 |
| `preview` | 展开所选图片或贴纸；大图通过 `preview` 上下文配置按键 |
| `home`、`end`、`left`、`right`、`backspace`、`delete`、`clear`、`delete_word` | 编辑器 |
| `filter`、`refresh`、`chat_info` | 导航；标题过滤、列表刷新、显示 ID |
| `folder_previous`、`folder_next` | 聊天列表文件夹 |
| `pin`、`pin_up`、`pin_down`、`archive` | 聊天列表；官方置顶排序和归档 |
| `reply`、`reply_target`、`open_link`、`next_action`、`previous_action`、`reveal` | 会话操作 |
| `chat_color`、`folder_color` | 外观选择器 |
| `pin`、`pins` | 会话；确认置顶/取消置顶，打开置顶列表 |
| `pins_more`、`pins_previous`、`unpin_all` | 置顶消息浮层（`pins` 上下文） |
| `search` | 打开本地搜索 |
| `search_scope`、`search_query`、`search_more`、`search_previous` | 搜索浮层 |
| `jump ALIAS` | 打开配置的稳定聊天 ID，返回导航状态 |
| `noop` | 移除指定上下文中的绑定 |

默认按键见[快捷键](Keybindings.md)，`colors` 表及应用内覆盖优先级见[外观](Appearance.md)。

导航模式的 `command` 动作打开[冒号命令](Commands.md)。新增 `command` 上下文，
可重绑 `complete_next`、`complete_previous`、`history_previous`、`history_next`、
`open`、`cancel` 及输入编辑动作。底栏的模式会显示 COMMAND。
命令栏默认用 Ctrl-C 取消；其他上下文仍用 Ctrl-C 退出。

## 附件输入

`attachments.auto_attach_images = true` 默认验证粘贴路径的图片头并暂存图片，设为 `false` 则保留为文字。其他类型的路径只有启用 `attachments.auto_attach_paths = true` 才会暂存；仍需主动发送。也可用 `:attach` 明确添加文件。列表操作见[附件](Attachments.md)。

`attachments.clipboard_as_photo = true` 将剪贴板图片默认作为 Telegram 照片，设为 `false` 则作为原文件。可在 `conversation`、`compose`、`attachments` 上下文重绑 `paste_clipboard`。

`attachments.terminal_clipboard = true` 在检测到支持时开启 OSC 5522 MIME 粘贴。SSH 快捷键用它读取终端所在主机的剪贴板，本机快捷键优先读系统剪贴板。设为 `false` 关闭此终端模式，使用原生剪贴板。见[终端](Terminal.md)。

消息编辑使用 `edit` 上下文。会话中的 `edit_message` 编辑所选消息，或继续该消息保存的本地编辑，`discard_edit` 丢弃编辑；`edit` 中支持 `send`、`cancel`、`newline` 和常规输入动作。底栏显示 EDIT，所选消息的编辑时间也显示在底栏。

会话动作 `delete_message` 打开删除确认；它与编辑器中删除下一个字符的 `delete`
不同。确认框使用 `overlay` 上下文中的 `up`、`down`、`open` 和 `cancel`；
底栏显示 DELETE，以及实际的范围选择和确认快捷键。

`conversation` 中的 `copy_text` 和 `copy_link` 操作明确选中的消息，默认键为 `y` 和 `Y`。
`attachments.terminal_clipboard` 控制 OSC 5522 媒体粘贴，不影响用户主动触发的 OSC 52
文字复制。本机文字复制复用已有原生剪贴板依赖。

`forward_message`（`f`）打开目标聊天补全，`save_message`（`g S`）准备转发给自己，
`saved_messages`（`g s`）打开 Saved Messages。转发预览使用 `forward` 上下文，
动作是 `send`（Enter）和 `cancel`（Esc/Ctrl-C）；底栏显示 FORWARD 和实际绑定。
长按按键产生的重复事件不会提交刚打开的转发预览。

桌面提醒使用 Lua 的 `notifications` 表，后端、预览、无声投递和合并方式见
[通知设置](Notifications.md)。它与底栏名为 `notifications` 的组件分别配置。

投票面板快捷键使用 `poll` 上下文，见[投票与测验](Polls.md)。

回应选择器使用 `reactions` 上下文，见[消息回应](Reactions.md)。

贴纸面板快捷键使用 `stickers` 上下文，见[贴纸](Stickers.md)。


## 运行中重载

`:config reload` 在后台校验整个文件，通过后一次替换快捷键、别名、颜色、布局、ghost text、
图标、通知和媒体粘贴设置。文件丢失、无法读取或配置错误时保留当前配置和草稿；`:status`
保留最近错误和配置路径。用 `return {}` 恢复默认。重载使用启动时选定的路径，不重读环境变量、
凭据或账号偏好文件。

延迟探测开关会直接传给当前 Telegram worker，无需重连。关闭后丢弃旧观测值，已提交的一次
RPC 可以完成。MIME 粘贴模式由现有终端 owner 更改；正在进行的终端粘贴会完整取消，
重载后重新粘贴即可。排队中的通知会取消，以免采用旧的隐私预览策略。

可绑定动作 `reload_config`。`?` 中包含所有有效快捷键与命令；帮助和 `:status` 都支持
j/k 和鼠标滚轮滚动。
