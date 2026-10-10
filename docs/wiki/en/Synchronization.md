# Cache and synchronization

[简体中文](../zh-CN/Synchronization.md) · [Guide](Home.md)

Termgram displays cached conversations before connecting to Telegram. Opening a
cached conversation reads its most recent local page while a network request
refreshes it. Messages remain visible during synchronization. Sending and server
operations wait for sign-in to finish.

Updates continue while history, dialog and other requests run in the background.
The currently viewed group also receives periodic difference checks using
Telegram's requested interval. Leaving the conversation or unfocusing the
terminal stops active polling. Ordinary push updates and gap recovery continue.

Automatic read receipts follow the displayed conversation. A focused terminal
acknowledges only through the highest incoming message whose bottom is visible,
after a successful frame. Opening a chat, loading history, or receiving a message
does not itself mark it read. Overlays, hidden conversation panes, and background
windows do not generate receipts. Visiting an older reply does not mark the
entire conversation read.

The server's incoming read boundary is cached with each chat. Receipts are
coalesced per chat and retried after failures; a late acknowledgement cannot
clear messages newer than its boundary. Remaining unread counts come from a
bounded request for that chat. If the count refresh fails or a newer message
arrives first, the displayed count may temporarily remain high until the next
server update or successful receipt. Older caches acquire the boundary from
Telegram before generating automatic receipts.

Each session has a separate `*.cache.sqlite3` database beside its session file.
The cache contains message text, chat metadata and downloaded attachment paths.
It does not contain authentication keys or user preferences. On Unix the file is
restricted to its owner. The database is not encrypted.

The cache contains messages Termgram has received or loaded, not the account's
entire Telegram history. Up to 100,000 message records are retained per account;
older records are discarded. An old gap that Telegram can no longer replay
invalidates the affected local history, which is fetched again when viewed.

To clear the cache, exit Termgram and remove that session's `.cache.sqlite3`
file. Media can be cleared separately by removing its `.media` directory. Keep
the `.session` file, `settings.conf` and `appearance.json` to preserve login and
preferences. Only one process may own a given account cache at a time.
The next launch reconnects and starts a fresh message cache.

Drafts live in a separate database: the base session path followed by
`.state.sqlite3` (for example, `termgram.session.state.sqlite3`). All account
slots share this file, with records keyed by Telegram user ID, chat and optional
topic ID. It contains local message text and reply targets, not authentication
keys, and is not encrypted. **Keep this file when clearing message caches.**
Drafts are excluded from message-cache eviction and gap invalidation.

Cached accounts load their drafts before showing conversations. SQLite reads
and coalesced writes run off the terminal thread. Normal exit flushes the latest
snapshot; a failed save is reported and retried while the app remains open. A
failed database transaction preserves the previous drafts. Draft text, replies
and Unicode cursor positions return together after a restart.

Full update batches and their covered cursor are committed in order. Restarting
after an interrupted write may replay updates, but must never skip uncommitted
messages. Session peer metadata is separate from this message checkpoint.

See [Telegram's update protocol](https://core.telegram.org/api/updates) for the
rules used by the underlying Grammers update engine. No end-to-end latency
comparison against Telegram Desktop has been measured yet.
