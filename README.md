# Termgram

Termgram is a focused, keyboard-first Telegram client for the terminal. It uses
Telegram's full MTProto user API—not the Bot API—so it can open your existing
direct messages and groups.

The first release deliberately does less:

- phone, login-code, and optional 2FA authentication;
- locally persisted Telegram session;
- direct-message and group conversation list with unread counts;
- recent history, live incoming messages, and read acknowledgements;
- plain-text messages with a Unicode-aware, multiline, per-chat draft;
- drag-and-drop upload of photos and arbitrary files;
- lazy download and opening of incoming photos, files, video, audio, and stickers;
- in-app navigation for supported Telegram chat and message links;
- sticker and custom-emoji Unicode fallbacks;
- wide split-pane and narrow single-pane layouts;
- chat filtering, contextual help, connection/error states, and safe terminal cleanup.

Broadcast channels are intentionally hidden. Telegram requires third-party
clients that display channels to implement additional sponsored-message
behavior, which is outside this essential messaging scope.

## Setup

Official GitHub release archives include the project's Telegram application
credentials from encrypted Actions secrets, so those binaries can go directly
to the login screen. The credentials identify the client application, not your
Telegram account; like any credentials compiled into a client binary, they can
be recovered by someone inspecting that binary. Your phone, login code, 2FA
password, and session are never placed in CI and remain local.

For a source build:

1. Create an application at [my.telegram.org/apps](https://my.telegram.org/apps),
   or use credentials for an application you already control.
2. Copy `.env.example` to `.env` and enter its API ID and API hash.
3. Run:

   ```sh
   cargo run --release
   ```

Termgram uses Rust 1.88, installed automatically by `rustup` from the pinned
`rust-toolchain.toml`. The session database defaults to the operating system's
application-data directory. It is effectively an account credential; keep it
private and never commit or share it. Set `TERMGRAM_SESSION` to override its
location. Upgrades continue to recognize `TUIGRAM_SESSION` and an existing
TUIGram session database, so renaming the application does not sign you out.

After the first build, launch the optimized client directly with:

```sh
./target/release/tg
```

On Windows, run `target\release\tg.exe`. GitHub Actions tests and packages the
client on Linux, macOS, and Windows. Every successful default-branch commit is
published as a prerelease with version `0.1.Z`, where `Z` is its first-parent
commit height. A commit whose subject starts with `release:` is published as a
normal release instead. Archives include both version and platform, for example
`termgram-0.1.42-linux.tar.gz`. Run `tg --version` to inspect a binary's version.

The public repository stores only the `TELEGRAM_API_ID` and
`TELEGRAM_API_HASH` secret names. GitHub encrypts their values and injects them
only into trusted default-branch builds, never pull-request builds.

## Lightweight by design

Termgram uses one async runtime thread, has no idle animation wake-up while the
interface is static, renders only visible rows, and bounds message histories,
queues, and network caches. It still keeps a private SQLite session so login
credentials and Telegram update state survive restarts.

## Keyboard

| Context | Keys |
| --- | --- |
| Anywhere | `Ctrl+C` quit, `Ctrl+L` redraw |
| Sign in | `Enter` submit, `Esc` restart from phone after code/2FA |
| Chats | `↑`/`↓` or `j`/`k` move, `Enter` open, `/` filter |
| Conversation | `PgUp`/`PgDn` scroll, `Home` oldest loaded, `End`/`G` latest |
| Message actions | click or use `o`/`O` to select; `Enter` downloads/reveals media, `l` follows its Telegram link |
| Wide layout | `Tab` switch pane |
| Narrow conversation | `Esc` return to chats |
| Composer | `i` or `Enter` start, `Enter` send, `Ctrl+J`/`Shift+Enter` newline, `Esc` preserve draft |
| Editing | arrows, `Home`/`End`, `Ctrl+A`/`Ctrl+E`, `Ctrl+W`, `Ctrl+U` |

`?` opens contextual help while navigating. In a conversation, `/` starts a
message so Telegram bot commands remain usable; chat filtering is only active
from the chat list.

Drag one or more files from the desktop into an open conversation and drop them
on the terminal. JPG, JPEG, PNG, and WebP files are sent as compressed Telegram
photos; other inputs are preserved as documents. Incoming media downloads only
when activated. Termgram saves it in a private per-process temporary directory
and sanitizes remote filenames. Activate the row again to reveal the file in
the operating system's file manager—Termgram never executes downloaded files.
Downloads remain available for the current session and are left to the operating
system's normal temporary-file cleanup.

Termgram follows public `t.me`/`telegram.me` and `tg://resolve` links in-app,
including message links. Private `t.me/c` and `tg://privatepost` links work for
groups already loaded in the conversation list. Invite links and broadcast
channel links remain outside the client's scope.

Run `cargo test` for the reducer, input, sanitization, wrapping, and rendering
behavior.

## Design

Termgram learns from [paul-nameless/tg](https://github.com/paul-nameless/tg)
without copying its breadth or its index-based shared mutable state. A typed,
single-owner reducer keeps selection, drafts, and scroll state stable while a
separate async Telegram worker handles network calls. The interface borrows the
calm interaction principles of modern Claude Code: a fixed composer, minimal
chrome, contextual help, explicit async states, overlays that own input, and a
real structural change on narrow terminals.

The client uses [grammers](https://codeberg.org/Lonami/grammers), a native Rust
MTProto implementation. Grammers states that its protocol/cryptography code has
not received a formal security audit. This project is unofficial and is not
affiliated with Telegram.

## Intentionally out of scope

Broadcast channels, reactions, sticker/GIF selection, calls, stories, contacts,
chat creation, replies, edits/deletes/forwarding, message search, group
administration, notifications, polls, and secret chats.

## Development

```sh
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets
cargo build --release
```

## Publishing

Pushing an ordinary commit to the default branch creates a GitHub prerelease
after every check and all three native builds pass. Release publication reuses
those exact tested artifacts and adds `SHA256SUMS`; a failed CI run never
publishes anything.

To promote the current tree as a stable release, create a release commit and
push it:

```sh
git commit --allow-empty -m "release: stable"
git push
```

Versions stay on the `0.1` line for now. `Z` advances once per first-parent
default-branch commit, so merge internals do not unexpectedly consume several
versions. Failed or interrupted publications are retried by the next push.
Existing tags are never moved; a height collision fails closed. Automatic
publishing starts at `0.1.2`, the commit that introduced this release pipeline.
