# Termgram

Termgram is a focused, keyboard-first Telegram client for the terminal. It uses
Telegram's full MTProto user API—not the Bot API—so it can open your existing
direct messages and groups.

The first release deliberately does less:

- phone/login-code, optional 2FA password, and QR-code authentication;
- up to eight locally persisted Telegram accounts with fast in-app switching;
- direct-message and group conversation list with unread counts;
- recent history, live incoming messages, and read acknowledgements;
- plain-text messages with a Unicode-aware, multiline, per-chat draft;
- native replies with a visible target, optimistic delivery, and exact retry;
- drag-and-drop upload of photos and arbitrary files;
- lazy download and opening of incoming photos, files, video, audio, and stickers;
- in-app navigation for supported Telegram chat and message links;
- sticker and custom-emoji Unicode fallbacks;
- optional terminal mouse input for chats, media, scrolling, settings, and accounts;
- wide split-pane and narrow single-pane layouts;
- chat filtering, contextual help, connection/error states, and safe terminal cleanup.

Broadcast channels are intentionally hidden. Telegram requires third-party
clients that display channels to implement additional sponsored-message
behavior, which is outside this essential messaging scope.

## Install

Prebuilt releases target x86_64 and ARM64 Linux, Apple silicon and Intel macOS,
and x64 and ARM64 Windows. The installers select the native architecture and
reject unsupported combinations before downloading a binary. Linux and macOS
also require Bash, `curl`, `tar`, and either `sha256sum` or `shasum`; Windows
requires PowerShell 5.1 or newer.

On Linux or macOS:

```sh
curl --proto '=https' --tlsv1.2 -sSfL \
  https://github.com/iebb/termgram/releases/latest/download/install.sh | bash
```

This installs `tg` in `~/.local/bin`. To install the newest prerelease, or use
another directory, pass noninteractive environment overrides to `bash`:

```sh
curl --proto '=https' --tlsv1.2 -sSfL \
  https://github.com/iebb/termgram/releases/latest/download/install.sh \
  | CHANNEL=prerelease INSTALL_DIR="$HOME/bin" bash
```

The shortest form pipes the download directly into the shell. To inspect the
installer first, download it and run the saved copy only after reviewing it:

```sh
curl --proto '=https' --tlsv1.2 -fLo termgram-install.sh \
  https://github.com/iebb/termgram/releases/latest/download/install.sh
less termgram-install.sh
bash termgram-install.sh
```

On Windows PowerShell:

```powershell
$installer = Invoke-RestMethod 'https://github.com/iebb/termgram/releases/latest/download/install.ps1'
& ([scriptblock]::Create([string]$installer))
```

The default destination is `%LOCALAPPDATA%\Programs\Termgram\bin`. Parameters
provide the same noninteractive overrides:

```powershell
& ([scriptblock]::Create([string]$installer)) -Channel prerelease -BinDir "$HOME\bin"
```

To inspect before executing:

```powershell
Invoke-WebRequest 'https://github.com/iebb/termgram/releases/latest/download/install.ps1' -OutFile termgram-install.ps1
Get-Content termgram-install.ps1
$installer = Get-Content termgram-install.ps1 -Raw
& ([scriptblock]::Create($installer))
```

Both installers default to the highest stable `0.1.Z` release. The prerelease
channel chooses the highest `0.1.Z` release of either type, so it never
downgrades past a newer stable release. Downloads come from the public
`iebb/termgram` repository and are checked against that release's
`SHA256SUMS`; the archive must contain only `tg` or `tg.exe`. Replacement is
atomic and fails without damaging an existing binary. The scripts do not ask
for elevation or edit `PATH`; if the destination is not already present, they
print the directory to add.

After installation, launch Termgram with `tg`. Update within the terminal with
`tg update`, or opt into the newest prerelease with `tg update --prerelease`.
Termgram checks the selected channel at most once per day in the background and
shows a quiet footer hint when an update is available; applying it is always an
explicit command. The updater trusts GitHub's HTTPS release delivery and uses
`SHA256SUMS` to verify download integrity before atomic replacement.
On Windows, replacement completes after the updating process exits. If another
`tg` process keeps the executable locked, the next launch shows a fixed warning
and `tg update` performs a fresh verified download.

Press `s` while navigating to open the small settings overlay. It stores four
non-sensitive preferences: automatic update checks, stable/prerelease channel,
temporary-download reveal behavior, and the optional right-aligned message-ID
column. The same private settings file remembers the selected account slot;
Telegram login credentials remain exclusively in separate session databases.

## Source build

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
location for Account 1; additional account sessions use a private `accounts`
subdirectory beside it. Upgrades continue to recognize `TUIGRAM_SESSION` and an
existing TUIGram session database, so renaming the application does not sign
you out.

After the first build, launch the optimized client directly with:

```sh
./target/release/tg
```

On Windows, run `target\release\tg.exe`. GitHub Actions tests and packages six
native targets across Linux, macOS, and Windows. Every successful
default-branch commit is published as a prerelease with version `0.1.Z`, where
`Z` is its first-parent commit height. A commit whose subject starts with
`release:` is published as a normal release instead. Archives include the
version and platform/architecture, for example
`termgram-0.1.42-linux-aarch64.tar.gz` or
`termgram-0.1.42-macos-x86_64.tar.gz`. The legacy `linux`, `macos`, and `windows`
asset names remain the x86_64 Linux, Apple-silicon macOS, and x64 Windows builds
so existing auto-updaters remain compatible. Run `tg --version` to inspect a
binary's version.

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
| Anywhere | `Ctrl+C` quit, `Ctrl+L` redraw, `F2` next account, `F3` add account |
| Sign in | `Enter` submit, `Tab` opens QR login or switches its compact/full-cell display, `Esc` return to phone |
| Chats | `↑`/`↓` or `j`/`k` move, `Enter` open, `/` filter |
| Accounts | `a` open picker, `↑`/`↓` select, `Enter` switch/add, `1`–`8` direct |
| Conversation | `PgUp`/`PgDn` scroll, `Home` oldest loaded, `End`/`G` latest |
| Message actions | click or use `o`/`O` to select each action; `Enter` activates media, links, and supported bot buttons; `l` follows the selected/first link |
| Message cursor | `[`/`]` select older/newer loaded messages, starting at the latest |
| Replies | `R` replies to the selected/latest message; right-click replies under the pointer; select an existing reply with `o`/`O`, then `r` jumps to its target |
| Wide layout | `Tab` switch pane |
| Narrow conversation | `Esc` return to chats |
| Composer | `i` or `Enter` start, `Enter` send, `Ctrl+J`/`Shift+Enter` newline; `Esc` cancels reply context first, then preserves the draft and leaves |
| Editing | arrows, `Home`/`End`, `Ctrl+A`/`Ctrl+E`, `Ctrl+W`, `Ctrl+U` |

`?` opens contextual help while navigating. In a conversation, `/` starts a
message so Telegram bot commands remain usable; chat filtering is only active
from the chat list. Press `s` to configure the message-ID column and the other
essential preferences.

When the terminal supports mouse reporting, clicking a chat opens it, clicking
media, a link, or an inline bot button activates it, right-clicking a message
starts a reply, and the wheel scrolls the pane under the pointer. Settings and
account rows are clickable too. Every mouse action has a keyboard equivalent,
so terminals without mouse reporting remain fully usable.

Account switching keeps Termgram lightweight: only one account is connected at
a time. Each account has an isolated private SQLite session. Press `a` from the
chat list for the picker, or use `F2`/`F3` even from login and error screens so
an unfinished new login never traps you away from an existing account.

During sign-in, Termgram shows a fixed progress message while Telegram checks a
phone number, login code, or 2FA password. Password entry stays masked. QR login
rotates its short-lived code automatically; scan it from Telegram on a device
where the account is already signed in. Its default display follows established
terminal QR tools: exact RGB black-on-white `█`/`▀`/`▄` cells, two QR rows per
terminal row, and a two-module quiet zone. It fits an 80 × 24 terminal; press
`Tab` for a larger compatibility display made only from colored spaces when
block glyphs render poorly. QR tokens, codes, passwords, and
phone numbers are never written to settings or logs.

Drag one or more files from the desktop into an open conversation and drop them
on the terminal. Text already in the composer becomes the first file's caption,
including when replying. JPG, JPEG, PNG, and WebP files are sent as compressed
Telegram photos; other inputs are preserved as documents. Incoming media downloads only
when activated. Termgram saves it in a private per-process temporary directory
and sanitizes remote filenames. Activate the row again to reveal the file in
the operating system's file manager—Termgram never executes downloaded files.
Downloads remain available for the current session and are left to the operating
system's normal temporary-file cleanup.

Termgram renders Telegram URL entities—including URLs hidden behind display
text—as explicit selectable rows. Public `t.me`/`telegram.me` and `tg://resolve`
targets open in-app; ordinary HTTP(S) targets open through the operating system
without a shell. Private `t.me/c` and `tg://privatepost` links work for groups
already loaded in the conversation list. Inline URL, web-view, callback, and
game buttons are supported; password-gated, payment, contact/location, and
peer-selection buttons are shown as requiring a graphical Telegram client.
Invite links and broadcast channel links remain outside the client's scope.

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
chat creation, edits/deletes/forwarding, message search, group
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
those exact tested artifacts, the two installation scripts, and `SHA256SUMS`;
a failed CI run never publishes anything.

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
