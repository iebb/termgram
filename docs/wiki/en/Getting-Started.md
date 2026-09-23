# Get started

[简体中文](../zh-CN/Getting-Started.md) · [Guide](Home.md)

## Install a release

Native releases cover Linux x86_64/ARM64, macOS Intel/Apple silicon, and Windows
x64/ARM64. Linux/macOS need Bash, `curl`, `tar`, and `sha256sum` or `shasum`.
Windows needs PowerShell 5.1 or newer. Use a terminal with at least 40 × 10 cells;
80 × 24 or larger is more comfortable.

```sh
curl --proto '=https' --tlsv1.2 -sSfL \
  https://github.com/iebb/termgram/releases/latest/download/install.sh | bash
```

The default destination is `~/.local/bin`, and the installed command is `tg`.
To inspect the installer before
running it, save the same URL to a file, read it, then run `bash install.sh`.
For a prerelease or another destination:

```sh
curl --proto '=https' --tlsv1.2 -sSfL \
  https://github.com/iebb/termgram/releases/latest/download/install.sh \
  | CHANNEL=prerelease INSTALL_DIR="$HOME/bin" bash
```

Homebrew users can install the same latest prerelease as
`brew install iebb/termgram/termgram-pre` from the in-repository tap (see the
[README](../../../README.md)); the `termgram-pre` formula is regenerated
automatically after each prerelease and conflicts with `termgram` because both
install `tg`.

Windows PowerShell:

```powershell
$installer = Invoke-RestMethod 'https://github.com/iebb/termgram/releases/latest/download/install.ps1'
& ([scriptblock]::Create([string]$installer))
# Optional:
& ([scriptblock]::Create([string]$installer)) -Channel prerelease -BinDir "$HOME\bin"
```

The Windows default is `%LOCALAPPDATA%\Programs\Termgram\bin`, and the installed
command is `tg` (`tg.exe`). Inspect `$installer`
first if desired. Installers verify the release's `SHA256SUMS`, select the native
architecture, and print the destination to add to PATH when needed. They do not
request elevation or edit PATH. Release selection and updates are described in
[Updates](Updates.md).

## Sign in and send a message

1. Run `tg`. Official release binaries include the project's Telegram application
   credentials. A source build needs your own credentials as described below.
2. Enter your phone in international format, then the login code and optional 2FA
   password. Password entry is masked. Tab on the phone screen starts QR login;
   scan it in Telegram under **Settings → Devices → Link Desktop Device**.
3. Move through chats with `j/k` or arrows, then Enter to open one.
4. Press `i`, type your message, and Enter to send. Ctrl-J adds a newline.
   Esc leaves the editor while keeping a local draft for this chat and account,
   including across normal restarts. See [drafts and replies](UX.md#compose-and-reply).
5. `G` returns to the newest messages; `?` opens help. `q` quits from navigation.

QR codes rotate automatically. Tab switches between compact block rendering and
larger full-cell rendering. Esc returns to phone sign-in. F2 switches to the next
existing account; F3 adds an account, including from login/error screens.
All listed keys are defaults and can be [configured](Configuration.md).

## Install from source with Cargo

The same Cargo command supports Linux x86_64/ARM64, macOS Intel/Apple silicon,
and Windows x64/ARM64. It builds the selected Git source with the committed
lockfile and installs `tg` (`tg.exe` on Windows) using Cargo's release profile.

### Set up the build tools once

Install [Rust through rustup](https://rustup.rs/) and Git, plus the native C
compiler/linker. Lua and SQLite are compiled with the application; separate Lua
or SQLite packages are not required.

| Platform | Native build tools |
| --- | --- |
| Linux | C compiler, linker, make and Git; on Debian/Ubuntu: `sudo apt-get install build-essential git` |
| macOS | Xcode Command Line Tools: `xcode-select --install` |
| Windows | Visual Studio C++ build tools and Windows SDK, with the MSVC tools for your CPU; ARM64 needs the ARM64 component. Follow the [rustup MSVC setup](https://rust-lang.github.io/rustup/installation/windows-msvc.html). |

Install the project's pinned compiler without changing your default toolchain:

```sh
rustup toolchain install 1.98.0 --profile minimal
```

### Install or update

This single command works in Bash, Zsh and PowerShell:

```sh
cargo +1.98.0 install --locked --git https://github.com/iebb/termgram --bin tg termgram
```

It installs the repository's default branch. Repeat it to update to the latest
source on that branch. `+1.98.0` selects the compiler explicitly, including when
you run the command outside a checkout. A fetched repository's toolchain file
does not select the Cargo process you already started.

Cargo normally installs to `~/.cargo/bin/tg` on Linux/macOS and
`%USERPROFILE%\.cargo\bin\tg.exe` on Windows. Ensure that directory is on PATH.
A custom `CARGO_HOME` or Cargo install-root setting can change the destination;
`--root DIR` installs into `DIR/bin`, so pass the parent of the desired bin folder.
If an older `tg` takes precedence, inspect `type -a tg` in Bash/Zsh or
`Get-Command tg -All` in PowerShell and remove the obsolete executable from PATH.

To select another branch, add `--branch BRANCH`; for a specific commit, use
`--rev COMMIT` instead. Replace these placeholders with the desired Git reference.
PR features require that PR's source branch until merged. The Git URL is required
for this installation route; do not substitute `cargo install termgram`, which
would look for a crates.io package.

For the exact version described by the published Wiki, open its **Documentation
source** footer and use that commit with `--rev COMMIT`. This avoids installing
the default branch while following guides for changes that are still in review.

`cargo uninstall termgram` removes the Cargo-managed executable, leaving your
Termgram configuration and account data. If you installed with `--root`, pass the
same root when updating or uninstalling. `--force` rebuilds the same revision,
for example after changing embedded application credentials. `--target-dir DIR`
can keep compilation artifacts for later source updates.

See [Cargo's installation reference](https://doc.rust-lang.org/cargo/commands/cargo-install.html)
for installation tracking and options, and [Updates](Updates.md) for the difference
between a Cargo source update and `tg update`.

### Telegram application credentials

Git source installations need your own application API ID and hash from
[my.telegram.org/apps](https://my.telegram.org/apps). These identify the client;
you still sign in with your Telegram account after starting `tg`. Source installs
do not receive the project's GitHub Actions release credentials.

Set these runtime variables before launching. In Bash/Zsh:

```sh
export TELEGRAM_API_ID=123456
export TELEGRAM_API_HASH=your_api_hash
tg
```

In PowerShell:

```powershell
$env:TELEGRAM_API_ID = '123456'
$env:TELEGRAM_API_HASH = 'your_api_hash'
tg
```

Replace the example values. Runtime variables override embedded defaults.
Alternatively, the app reads `.env` from its working directory or a parent.
For a personal binary that works without runtime variables, set
`TERMGRAM_EMBEDDED_API_ID` and `TERMGRAM_EMBEDDED_API_HASH` in the build environment
before running Cargo. Those application credentials become part of the executable;
your account session is never embedded.

### Install a local checkout

For development, clone the repository and optionally copy `.env.example` to
`.env` for runtime credentials, then install the checkout:

```sh
git clone https://github.com/iebb/termgram.git
cd termgram
cargo install --locked --path . --bin tg
```

This uses the checkout's pinned toolchain. `cargo run --locked --release` runs it
without installing. Do not commit `.env` or session databases.

Use [configuration](Configuration.md) for paths and overrides,
[terminal integration](Terminal.md) for graphics/key issues, and
[synchronization](Synchronization.md) for offline/cache behavior.
