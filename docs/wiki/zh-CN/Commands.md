# 冒号命令

[English](../en/Commands.md) · [使用指南](Home.md)

在聊天列表或会话的导航模式按 `:`，底部会显示命令、说明、参数用法和实际生效的快捷键。
不可用操作会说明缺少的选择或连接条件。标题保留打开命令栏时的账号和操作对象。
在输入框、搜索框、聊天筛选框或登录框输入 `:` 时，它仍是普通文字。

| 按键 | 行为 |
| --- | --- |
| Tab / Ctrl-N | 补全下一个候选 |
| Shift-Tab / Ctrl-P | 补全上一个候选 |
| Up / Down | 召回匹配当前输入前缀的较早/较新命令 |
| Enter | 执行完整命令；空输入时关闭 |
| Esc / Ctrl-C | 取消并返回会话 |
| Left/Right、Home/End、Ctrl-A/Ctrl-E | 移动光标 |
| Backspace/Delete、Ctrl-W、Ctrl-U | 编辑或清空输入 |

鼠标点击候选只填入命令，不会执行。缺少参数或出错时保留输入，继续编辑。
只执行完整命令名及明确提供的 `h`、`q` 别名；例如 `qui` 需要先补全再执行。
历史按账号在进程内保留，最多各 64 条，不写入磁盘。多行粘贴不会自动执行一串命令。

| 命令 | 行为 |
| --- | --- |
| `help [命令]`、`h` | 浏览全部命令或查看某条命令说明 |
| `chat <别名、ID 或名称>` | 打开当前账号的缓存聊天；Tab 筛选名称及 Lua 别名 |
| `folder <ID 或名称>` | 选择 All chats、Archive 或 Telegram 文件夹 |
| `account [槽位]` | 打开账号选择器，或切换到已有账号槽位 |
| `search [正则]` | 打开本地搜索，或在当前本地范围提交原样正则 |
| `search --cloud [筛选] [文字]` | 搜索当前聊天的云端历史，按发送者、UTC 日期、媒体筛选（[语法](Search.md)） |
| `attach <paths...>` | 将本地文件加入目标聊天草稿，不会自动发送 |
| `paste` | 将剪贴板的文件、图片或文字加入目标聊天草稿 |
| `attachments` | 查看当前会话草稿中的附件 |
| `latest` | 回到已打开会话的最新消息 |
| `unread` | 定位到聊天已读边界后的首条入站消息 |
| `read` | 明确将目标聊天全部标读，并清除未读提醒 |
| `mark-unread` | 设置 Telegram 的未读提醒，不倒退消息回执 |
| `edit` | 编辑所选已送达消息；若该消息有保存的编辑则继续编辑 |
| `edit-discard` | 丢弃当前聊天的本地编辑，保留普通草稿 |
| `delete` | 检查所选消息，明确选择删除范围后确认 |
| `copy [text, link]` | 复制所选消息正文/说明（默认），或 Telegram 消息链接 |
| `forward <chat or saved>` | 用 Tab 选择目标，再预览所选消息并确认转发 |
| `save` | 预览并将所选消息原生转发到当前账号的 Saved Messages |
| `saved` | 打开 Saved Messages，即使它尚未出现在最近聊天中 |
| `open <@username or Telegram link>` | 查找并打开缓存列表之外的用户、群组或消息（[说明](Chats.md)） |
| `join <invite link>` | 预览邀请，明确确认后加入或申请加入（[说明](Chats.md)） |
| `info` | 查看聊天资料、权限和慢速模式（[说明](Chats.md)） |
| `reply` | 回复明确选中的消息 |
| `poll` | 查看选中的[投票或测验](Polls.md)，明确提交后投票 |
| `react` | 添加或移除选中消息的普通 emoji [回应](Reactions.md) |
| `spoiler` | 揭示或隐藏选中消息的剧透 |
| `quote` | 展开或收起可折叠引用 |
| `preview` | 放大选中的图片或贴纸 |
| `reveal` | 必要时下载，并在 Finder、Explorer 或文件管理器中定位附件 |
| `pins` | 浏览当前会话的置顶消息 |
| `pin chat`、`unpin chat` | 设置聊天在打开命令栏时的文件夹中的置顶状态 |
| `pin message`、`unpin message` | 设置选中消息的置顶状态，沿用 Telegram 选项确认界面 |
| `mute [duration]`、`unmute` | 修改捕获聊天的 Telegram 通知状态（[说明](Notifications.md)） |
| `mentions` | 浏览目标聊天的未读提及及回复给你的消息（[说明](Notifications.md)） |
| `archive`、`unarchive` | 将指定聊天归档，或从 Archive 恢复 |
| `sidebar [show、hide 或 toggle]` | 显示、隐藏或切换侧栏；省略参数时切换 |
| `color chat`、`color folder` | 打开对应颜色选择器 |
| `settings` | 打开应用设置 |
| `config reload`、`reload` | 校验后整体应用 Lua 配置；失败时保留当前设置 |
| `status` | 查看连接、DC、最近 Ping、缓存加载、配置及剪贴板能力 |
| `refresh` | 刷新聊天和文件夹列表 |
| `quit`、`q` | 正常退出 |

直接执行聊天名称时，需要唯一且完整匹配。输入部分名称后用 Tab 选取 ID；同名聊天不会猜测。
补全使用已加载数据，输入时不会发送网络搜索。聊天列表因新消息重排时，置顶和归档仍使用
打开命令栏时的稳定对象。重复执行置顶或归档命令会保留期望状态，不会反向切换。

`search` 后第一个空格之后的内容按原样作为正则，包括反斜杠和末尾空格，不使用 shell 引号规则。明确的 `--cloud` 前缀启用 Yazi 平台参数引号与类型化筛选；两种模式均不执行 shell。
`status` 只读取现有观测值：没有 DC 或 Ping 已过期时显示不可用，
不会显示成零；页面也区分内存中的消息和持久缓存的搜索覆盖范围。

可在 `chats` 和 `conversation` 上下文重绑入口动作 `command`。`command` 上下文提供
`complete_next`、`complete_previous`、`history_previous`、`history_next`，以及普通编辑动作。
参见 [Lua 配置](Configuration.md)。快捷键和命令共用业务动作与说明。

`:config reload` 和 `:reload` 原子重载 Lua 文件，错误时保留当前配置。`:status` 显示配置路径、
修订次数、最近错误、通知策略、媒体粘贴能力和构建身份，详见[配置](Configuration.md)。
