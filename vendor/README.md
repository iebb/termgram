# Upstream code

Command metadata and candidate layout in `src/commands.rs` and
`src/ui/commands.rs` follow the finite command registry in Codex's
`codex-rs/tui/src/slash_command.rs` and `bottom_pane/command_popup.rs` at
`78245b47af2a7aafcabe025828ceecca69db4df1`. These are original application
adapters: no Codex source or plugin runtime is copied. The existing Yazi key
parser, Termgram `TextInput` and Ratatui widgets retain input/rendering ownership.

Conversation shading in `src/transcript.rs` follows the theme-relative design
of Codex `codex-rs/tui/src/style.rs` at
`78245b47af2a7aafcabe025828ceecca69db4df1` (https://github.com/openai/codex).
It is a small original adapter, not copied source: Yazi's existing background
report supplies the color, with no Codex probing code or additional input reader.

Yazi is pinned to `9203fd2604f867ab5ec18f24203b918975c4c00a` from
https://github.com/sxyazi/yazi. All Yazi crates except `yazi-term` and `yazi-tty` are Git
dependencies at that revision.

`yazi-term/` copies the complete upstream `yazi-term/src` and its MIT license.
Its standalone manifest resolves upstream workspace dependencies explicitly.
Local source changes expose mouse coordinates/modifiers, key state,
`KeyEvent::new`, and clipboard payload access to Rust consumers. Clipboard reads
retain the OSC 5522 request ID in parsed success and error events, reject
oversized or inconsistent IDs, and include it in the existing aggregate budget.
The upstream parser, platform reader, size limits, timeouts, and restoration
lifecycle remain in use.

`yazi-tty/` contains the complete upstream crate source and MIT license at the
same revision, with an explicit standalone manifest. `ReadClipboard::new` and
`with_id` provide a Rust construction path for the existing sequence formatter;
the optional ID is restricted to the protocol's character set and 128 bytes.
The Lua path retains its previous behavior with an empty ID. All other sequence
and TTY access code is unchanged. This small API patch avoids copying the
formatter or creating a separate parser or Lua host in Termgram.

When updating, replace from one upstream revision, reapply these visibility
changes, update all Yazi Git revisions together, and review the integration in
`src/terminal.rs`. Keep vendored formatting and do not run Termgram's lint or
formatting policy over upstream source.

The integration in `src/terminal.rs` adapts the mode setup/cleanup sequences from
`yazi-tui/src/raterm.rs` and the two-stage probe handling from
`yazi-actor/src/app/report.rs` and `app/passthrough.rs` at the same revision.
These are covered by `licenses/Yazi-MIT.txt`. Termgram makes tmux option changes
explicitly opt-in and removes the actor/proxy dispatch layer. The upstream
`yazi-emulator`, `yazi-tty`, parser, reader, and platform restorer remain in use.

`yazi-term/LICENSE` retains the separate Michael Davis and sxyazi attribution.

`yazi-image/icc.rs` copies `yazi-adapter/src/icc.rs` unchanged from the same
Yazi revision, under `licenses/Yazi-MIT.txt`. This private upstream helper retains
ICC-to-sRGB conversion while `ratatui-image` owns multi-image encoding and clipped
rendering. `src/media.rs` also follows Yazi's EXIF orientation handling and Kitty
cleanup sequence. Protocol selection still uses the original `Drivers::matches`;
old Kitty-only implementations and external-overlay drivers fall back to the
library's Unicode half-blocks. The former single-surface adapter initialization
and embedded Yazi presets are no longer needed.

`grammers-session/` vendors crates.io 0.10.0 from
https://codeberg.org/Lonami/grammers at
`5c6d44ff30e02d6c9295bcf1fcb51403ad77c981` (`grammers-session/`).
The original source, tests, MIT and Apache licenses are retained. A narrow patch
in `src/storages/sqlite.rs` sets a five-second SQLite busy timeout, uses immediate
write transactions, holds the connection mutex through home-DC transactions,
and creates the schema and its version in one serialized transaction. The
session format is unchanged. SQLite's existing rollback journal is retained;
WAL is not necessary for multi-process operation, and the bundled libSQL SQLite
engine predates upstream's WAL-reset fix. Reapply and review these changes when
upgrading the session crate. `tests/session_concurrency.rs` validates independent
processes opening and writing one session, including initial schema creation.

`src/message_box/mod.rs` also follows Telegram's documented startup recovery:
only common/secondary differences start immediately; channel differences are
requested when Telegram reports a gap, including `updateChannelTooLong`.
That notification preserves an existing local channel PTS instead of replacing
it with the remote PTS before recovery. The upstream connection-flow assertion
is adjusted; `tests/update_recovery.rs` checks both integration boundaries.
See https://core.telegram.org/api/updates#recovering-gaps.

`grammers-client/` vendors crates.io 0.10.0 from the same Grammers revision
(`grammers-client/`). Original source, examples, tests and both licenses are
retained. `src/client/updates.rs` is adapted:

- `next_batch` drains a complete update batch and returns its covered cursor;
- `restore_state` loads the application's durable cursor before polling;
- too-long differences emit a raw `PtsChanged` / `ChannelTooLong` invalidation
  before replacement messages, allowing the local store to discard stale data.

`src/telegram/local.rs` persists events before committing their cursor and reads
that cursor on restart. Telegram's session peer cache remains separate: dialog
iteration may advance its PTS without having cached any message content.

Active-group polling uses the same message-box engine. `set_active_channel`
seeds only unknown PTS, recovers the viewed group immediately, and schedules its
next final difference using the server timeout (one second when absent). Leaving
the group restores the normal recovery deadline. A watch channel wakes the
existing update receive without cancellation or a second update consumer.
The deadline helper also accounts for deadlines moved earlier than the current
minimum. `tests/update_recovery.rs` covers activation, timeout and deactivation.

`libsql/` vendors the published libSQL 0.9.30 Rust crate from
https://github.com/tursodatabase/libsql at
`0653c5788d77ef16a97c56ff3e9fdc11717a72d9` (`libsql/`), with the upstream
repository's `LICENSE.md`. The published standalone manifest, lockfile and original
source, examples and tests are retained; Cargo download metadata is omitted. An
empty workspace table allows the upstream tests to run independently of Termgram.
The root lockfile resolves the application dependency.

The only source changes backport upstream PR #2282 at
`0070ff3331cd6d09425b812e1cd3ebe32e1d4206`, including its regression test:
https://github.com/tursodatabase/libsql/pull/2282.
`src/local/connection.rs` makes disconnect idempotent, and `src/local/impls.rs`
removes the redundant connection destructor. This prevents a second
`sqlite3_close_v2` call on freed memory, reported in upstream issue #2251 and
consistent with Termgram's Windows ARM64 session-writer access violations.
The native SQLite engine and session format are unchanged. Directly using that
upstream Git revision would also require its 0.10.0-pre.4 engine upgrade; this
backport keeps the fix bounded to connection ownership. Remove this patch when
a stable release includes the fix. Keep upstream formatting and use its focused
`drop_closes_the_handle_exactly_once` test to validate the ownership boundary.

```sh
cargo test --locked --manifest-path vendor/libsql/Cargo.toml \
  --no-default-features --features core --lib \
  drop_closes_the_handle_exactly_once --target-dir target/libsql-regression
```

Attachment path arguments in `src/staging.rs` call the pinned `yazi-shared`
`shell/unix.rs` and `shell/windows.rs` implementations directly. Native Windows
quoting and Unix quoting remain upstream responsibilities. The former local
shell-like parser is removed. The file URL adapter uses the `url` crate; there
is no shell execution, variable expansion or copied argument parser.

Native clipboard handling in `src/clipboard.rs` directly depends on arboard
3.6.1 (`image-data`, `wayland-data-control`). File-list priority and the WSL
PowerShell fallback follow the approach in Codex `clipboard_paste.rs` at
`78245b47af2a7aafcabe025828ceecca69db4df1`; no Codex source is copied. Termgram
preserves all original file references, owns PNG assets with persistent drafts,
and bounds blocking work and child output/time. The WSL adapter uses constant
PowerShell source and JSON, not interpolation of clipboard values into commands.
The Wayland dependency retains its default Rust backend; `native_lib` and `dlopen`
are not enabled, so the existing Linux musl builds do not require new Wayland
C development packages. Actual clipboard availability is desktop-dependent.

Message editing in `src/telegram/editing.rs` calls Grammers 0.10.0
`Client::edit_message` directly. `grammers-mtsender/src/sender.rs` already routes
RPC `OwnUpdate` results through the existing PTS-ordered update stream, so edits
need no second fetch/reader or SDK patch. Eligibility follows Desktop
`history/history_item.cpp` (`canBeEdited`, `isTooOldForEdit`) and
`data/data_peer.cpp` (`canEditMessagesIndefinitely`) at
`4d4da471fbee771c10e173a83c003ba1728989f1`; no Desktop source is copied.
The UTF-16 range adapter retains untouched entities and enclosing styles while
dropping partially replaced or changed semantic targets.

`grammers-mtsender/` retains the complete published 0.10.0 crate and both licenses
from the same Grammers revision (`grammers-mtsender/`). An empty workspace table
keeps upstream regression tests independently runnable. The single source patch
in `src/sender.rs` retains the requested IDs for `messages.deleteMessages` when
routing its `AffectedMessages` RPC result. It emits the existing `UpdateShort`
/ `UpdateDeleteMessages` representation with the session adaptor's NO_DATE
sentinel, so application tombstones are committed before their covered PTS.
Request decoding now validates and consumes the TL constructor before reading
bare function fields, correcting the existing channel-delete/short-send decoder
as well. Content acknowledgements from `messages.readMessageContents` similarly retain
their IDs as `UpdateReadMessagesContents`. Other affected-message RPCs retain their
original path. The focused unit test checks both revoke scopes, channel PTS
ownership, content receipts and an unrelated RPC;
`tests/update_recovery.rs` verifies delivery together with the durable cursor.
Reapply this narrow patch until an upstream release retains deletion and content-receipt IDs.
Deletion eligibility in `src/telegram/deletion.rs` follows Desktop's `canDelete`
and `canDeleteForEveryone` at the revision recorded above; no source is copied.

`grammers-mtsender/src/configuration.rs` adds `proxy_fallback` behind the
existing `proxy` feature: when enabled, `src/sender_pool.rs` tries the
configured proxy first with a ten-second timeout and then the same datacenter
address directly, falling back only on I/O errors or that timeout. Without
the flag, a configured proxy is used exclusively. `src/net/tcp.rs` additionally
accepts `http://` proxy URLs and negotiates a plain HTTP CONNECT tunnel
(`Proxy-Authorization: Basic` included, response validated, no extra
dependencies); upstream only accepts `socks5`. Reapply when upgrading the
crate or once upstream accepts an equivalent option.

Message text copying calls the existing arboard 3.6.1 dependency. The persistent
Linux owner and native/terminal fallback behavior follow Codex
`codex-rs/tui/src/clipboard_copy.rs` at the Codex revision above; no source is
copied. The original adapter in `src/clipboard/copy.rs` runs one bounded writer,
serializes native reads/writes and passes WSL text through stdin with constant
PowerShell code. SSH writes only to the attached terminal. `src/terminal/clipboard.rs`
formats OSC 52 with Yazi's existing `SetClipboard`, following `yazi-widgets/src/clipboard.rs`
at the pinned revision, and flushes it through the same TTY on the main thread.
No clipboard reader, escape encoder or Linux selection implementation is copied.

`grammers-client/src/client/messages.rs` exposes
`forward_messages_with_random_ids` while keeping `forward_messages` as the
existing random-ID-generating wrapper. The request builder, attribution flags,
RPC and result mapping are unchanged. Termgram supplies one OS-random ID for a
reviewed message and retains it across retries. This avoids duplicating the SDK's
forward request or generating a new delivery intent after a transient error.
`getrandom` 0.4.3, already present transitively, is now a direct dependency.

`grammers-client/src/client/messages.rs` also exposes `SearchIter::sent_by(PeerRef)`,
a small setter for the existing `messages.search.from_id` field. Search query,
media/date filters, bounded iteration and pagination remain the SDK implementation
at the same pinned Grammers revision. Termgram's cloud command arguments reuse
the pinned Yazi platform tokenizer and Clap 4.6.7 (already a transitive dependency);
local regex queries never pass through either parser.

Telegram mute changes follow the peer-override lifecycle in Desktop's
`Telegram/SourceFiles/data/notify/data_peer_notify_settings.cpp` at
`4d4da471fbee771c10e173a83c003ba1728989f1`: read the complete peer settings,
change mute time, preserve unrelated fields, and synchronize the returned state.
`src/telegram/notifications.rs` is an original typed-RPC adapter; no Desktop code
is copied. The shared RPC coordinator rejects stale mute snapshots after a
notification-settings update and refreshes authoritative dialogs.

Unread-mention pagination adds `Client::unread_mentions` / `UnreadMentionIter` in
`grammers-client/src/client/messages.rs`. It reuses `IterBuffer`, `fill_buffer`,
peer mapping, limits and offset handling for `messages.getUnreadMentions`; no
second paging engine or update reader is added. Application navigation reuses
cloud-search cancellation and its existing result/context panel. The wrapper
currently queries the entire chat; topic scoping will be added with topics.

Mention receipts follow Desktop's `history/history_widget.cpp` visibility rule
and `history/history_item.cpp` unread-media distinction at the Desktop revision
above. Text mentions use `messages.readMessageContents` (ordered PTS result) or
`channels.readMessageContents` (Bool acknowledgement). Voice, round-video and TTL
media require explicit consumption. This is an original adapter, not copied
Desktop source. Cache receipts update existing JSON and protect only snapshots
still in flight, including messages not yet cached when the receipt arrived.

Desktop notification delivery depends on notify-rust 4.18.0 directly, with its
pure-Rust zbus backend for Unix desktops. Existing quick-xml 0.42.0 is a direct
Unix dependency for literal XML body escaping. Native delivery has one bounded
background worker; it does not implement platform notification protocols.
The legacy macOS adapter uses `schedule_raw` at the current instant because
`show` defers to Drop and hides send errors in this crate version. No native
callback waits or notification-center event loop are introduced.

The terminal OSC 9/BEL choice follows Codex `codex-rs/tui/src/notifications/{mod,
osc9,bel}.rs` at `78245b47af2a7aafcabe025828ceecca69db4df1`. The original application
adapter uses existing Yazi `EMULATOR`, `TTY` and `TmuxPassthrough`; no Codex source,
terminal detection or input handling is copied. Quiet messages do not fall back
to protocols that cannot suppress sound. Grouping is implemented before delivery
because replacement IDs do not work uniformly across the native backends.

Eligibility follows Desktop `notifications/notifications_manager.cpp` at the
Desktop revision above: unmuted chats notify, muted-chat mentions may use their
sender's notification state, and silent messages/sound-none remain quiet. Server
preview preferences apply independently. This is an original typed-RPC adapter,
not copied GPL source. Resolved settings expire and are invalidated by live
updates; notification-only metadata is never serialized into the message cache.

## Message entity rendering references

Telegram entity offsets use UTF-16 on the original text. Termgram converts them
while sanitizing to safe UTF-8, keeping this conversion separate from its cached
domain model and Ratatui renderer. The shared grapheme wrapper now returns source
ranges to retain styles and code whitespace. This is an extension of Termgram's
existing wrapper, not a copied Markdown renderer.

Reviewed Codex `codex-rs/tui/src/wrapping.rs` and `markdown_render.rs` at
`78245b47af2a7aafcabe025828ceecca69db4df1` for source-offset projection, styled spans,
quote prefixes and code whitespace. No Codex code or runtime was copied. The
existing Ratatui and unicode-segmentation dependencies supply styles and grapheme
boundaries. Telegram's entity protocol supplies spoiler and collapsed-quote
semantics: https://core.telegram.org/api/entities.

Polls use Grammers 0.10.0 typed TL requests and its existing ordered update
stream, with no SDK changes. Partial-result merging and visible-poll refresh
follow `Telegram/SourceFiles/data/data_poll.cpp` and
`Telegram/SourceFiles/api/api_polls.cpp` in Telegram Desktop at
`4d4da471fbee771c10e173a83c003ba1728989f1`; no Desktop source is copied.
The application preserves choices omitted by minimal updates and validates
stable answer bytes against a freshly fetched poll. The existing Ratatui entity
renderer, Yazi key resolver and single event loop own display and input.

Ordinary reactions call Grammers `Client::send_reactions` with `InputReactions`
from a vector, preserving the identity and chosen order of existing reactions.
Available emoji, chat permissions and limits use typed TL requests at the same
SDK revision. Partial-result merging, oldest-choice replacement and batched
visible refresh follow Desktop `data/data_message_reactions.cpp` at
`4d4da471fbee771c10e173a83c003ba1728989f1`; no source is copied. Polls and reactions
share the bounded cache journal and the existing message-view visitor. Neither
adds a terminal reader or an independent Telegram update consumer.
