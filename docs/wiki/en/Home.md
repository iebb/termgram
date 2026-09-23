# Termgram guide

[简体中文](../zh-CN/Home.md) · [Language selection](../Home.md)

Termgram uses Telegram's MTProto user API to access your direct messages and
groups. It supports up to eight account slots, with one account connected at a
time. Cached messages are available while it reconnects.

| I want to… | Read |
| --- | --- |
| Install, sign in, or build from source | [Get started](Getting-Started.md) |
| Learn the keys or migrate older bindings | [Keybindings](Keybindings.md) |
| Browse commands and complete chat or folder names | [Commands](Commands.md) |
| Remap keys, jump to a saved chat, change ghost text | [Lua configuration](Configuration.md) |
| Read, reply, send files, and switch accounts | [Daily workflows](UX.md) |
| Open a username, join an invitation, or inspect chat permissions | [Chat discovery and details](Chats.md) |
| Navigate Telegram folders | [Folders](Folders.md) |
| Pin chats or messages, browse pins | [Pins](Pins.md) |
| Read polls and quizzes, vote or retract a vote | [Polls](Polls.md) |
| Add, remove and read message reactions | [Reactions](Reactions.md) |
| Send stickers from Recent, Favorites or installed sets | [Stickers](Stickers.md) |
| Search local or cloud history with filters | [Message search](Search.md) |
| Desktop alerts, chat mute and unread mentions | [Notification settings](Notifications.md) |
| Color a chat or folder | [Appearance](Appearance.md) |
| Preview or reveal a file | [Attachments](Attachments.md) |
| Understand offline behavior or clear the cache | [Cache and synchronization](Synchronization.md) |
| Troubleshoot graphics, keys, or tmux | [Terminal integration](Terminal.md) |
| Update the app or understand releases | [Updates](Updates.md) |
| Contribute and maintain the implementation | [Development](Development.md) |

These pages describe their source revision; check the release you installed
with `tg --version`. The app's `?` help reflects your actual Lua bindings.
Documentation is available in English and Simplified Chinese; the interface
currently uses English text, with configurable composer ghost text.

Broadcast channels, secret chats, calls, stories, GIF pickers,
server-wide message search, contact management, creating groups and group
administration are outside this revision’s scope. Forum-topic navigation, cloud
drafts, simultaneous account connections, durable send queues, albums, scheduled
or silent sending and typing indicators are also not implemented.
Message editing, deletion, copying, forwarding, [stickers](Stickers.md) and
Saved Messages are supported; see [daily workflows](UX.md).
Folders are created and edited in an official client. Supported messages, links
and bot buttons are described in [daily workflows](UX.md).
