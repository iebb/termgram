# Updates and releases

[简体中文](../zh-CN/Updates.md) · [Guide](Home.md)

Run `tg update` for the selected channel, `tg update --stable` for stable, or
`tg update --prerelease` to include prereleases. The stable channel selects the
highest stable `0.1.Z`; the prerelease channel selects the highest `0.1.Z` of
either type, so a newer stable release is not replaced by an older prerelease.

Automatic checking runs in the background at most once per day for the selected
channel and shows a footer hint. Applying an update requires the explicit
command. `s` opens settings to disable checking or change channels. The updater
verifies `SHA256SUMS` from GitHub HTTPS delivery before replacing the executable.
On Windows replacement finishes after the updater exits. Another process can
keep the binary locked; a later `tg update` downloads a freshly verified copy.

`tg --version` shows the version, commit, branch, OS, architecture and build
number. A `*` marks a source tree with non-ignored changes. Piped version output
is plain text; `NO_COLOR=1` or `TERM=dumb` also disables its styling. Source
archives without Git metadata show `unknown` for unavailable fields.

## Cargo source installs

Repeat the [Cargo installation command](Getting-Started.md) to rebuild from the
latest source, keeping the same `--branch`, `--rev` or `--root` options when used.
A fixed `--rev` stays on that commit until you change it. Cargo tracks the Git
source even though the manifest's base package version remains `0.1.0`.

Git source builds display `0.1.Z` using their first-parent commit height. Check
the displayed commit to identify the source; Cargo's checkout can show a local
branch such as `master` or `HEAD` instead of the requested source branch. Archives
without Git metadata fall back to the manifest
version; CI can set an explicit version.

`tg update` downloads a prebuilt GitHub release; it does not run Cargo or follow
your source branch. Use the Cargo command to continue following source changes.

## Maintainer release workflow

CI checks formatting, strict Clippy, tests, installers, and six native packages:
Linux x86_64/ARM64, macOS Intel/Apple silicon, Windows x64/ARM64. Default-branch
builds receive application API credentials from the `TELEGRAM_API_ID` and
`TELEGRAM_API_HASH` Actions secrets; PR builds do not receive them.

Every successful default-branch commit is published as `0.1.Z`, where `Z` is its
first-parent height. Ordinary commits produce prereleases. Package-metadata
commits (subjects beginning with `chore(release):`) are not published; they only
retarget the in-repository Homebrew and Scoop files at the latest stable release
and the latest prerelease. A subject beginning
with `release:` produces a stable release; for example:

```sh
git commit --allow-empty -m "release: stable"
git push
```

Publication reuses tested artifacts and includes installers and `SHA256SUMS`.
Failed checks do not publish. Interrupted publications are retried on a later
push; existing tags are never moved. Automatic publication begins at height 2.
The legacy asset labels `linux`, `macos`, and `windows` mean Linux x86_64,
Apple-silicon macOS, and Windows x64; other architectures have explicit suffixes.

Application API credentials can be recovered from a compiled binary; they are
not a user's Telegram session. User phone numbers, login codes, passwords and
session files are never supplied to CI. This project is unofficial and is not
affiliated with Telegram.
