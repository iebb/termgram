# 快速入门

[English](../en/Getting-Started.md) · [指南首页](Home.md)

## 安装发行版

提供 Linux x86_64/ARM64、macOS Intel/Apple silicon、Windows x64/ARM64 原生构建。
Linux/macOS 需要 Bash、`curl`、`tar`，以及 `sha256sum` 或 `shasum`；Windows 需要
PowerShell 5.1 或更高版本。终端至少 40 × 10，建议使用 80 × 24 或更大窗口。

```sh
curl --proto '=https' --tlsv1.2 -sSfL \
  https://github.com/iebb/termgram/releases/latest/download/install.sh | bash
```

默认安装到 `~/.local/bin`，安装后的命令为 `tg`。如果希望先审阅脚本，可把相同 URL 下载为文件，阅读后执行
`bash install.sh`。选择预发布版或更改目录：

```sh
curl --proto '=https' --tlsv1.2 -sSfL \
  https://github.com/iebb/termgram/releases/latest/download/install.sh \
  | CHANNEL=prerelease INSTALL_DIR="$HOME/bin" bash
```

Homebrew 用户也可以通过仓库内置 tap 安装相同的最新预发布版：
`brew install iebb/termgram/termgram-pre`（见 [README](../../../README.md)）。
`termgram-pre` formula 在每次预发布后自动重新生成，与 `termgram` 冲突，因为两者都安装 `tg`。
macOS 上也可以用 cask 安装同一预发布版：`brew install iebb/termgram/termgram@pre`。

Windows PowerShell：

```powershell
$installer = Invoke-RestMethod 'https://github.com/iebb/termgram/releases/latest/download/install.ps1'
& ([scriptblock]::Create([string]$installer))
# 可选参数：
& ([scriptblock]::Create([string]$installer)) -Channel prerelease -BinDir "$HOME\bin"
```

Windows 默认目录为 `%LOCALAPPDATA%\Programs\Termgram\bin`，安装后的命令为 `tg`（`tg.exe`），也可先查看 `$installer`
内容。安装器会校验发行版 `SHA256SUMS`、选择原生架构，并在需要时提示加入 PATH 的目录；
不会提权或自行修改 PATH。版本选择与升级规则见[更新](Updates.md)。

## 登录并发送消息

1. 执行 `tg`。官方发行包内置项目的 Telegram 应用凭据；源码构建需要下文所述的自有凭据。
2. 输入国际格式手机号、验证码以及可选的两步验证密码。密码输入会掩码显示。
   手机号页按 Tab 使用二维码，再用已登录的 Telegram 在 **设置 → 设备 → 连接桌面设备** 扫码。
3. 用 `j/k` 或方向键选择聊天，Enter 打开。
4. 按 `i` 进入输入框，输入消息后 Enter 发送，Ctrl-J 换行。
   Esc 退出输入，为当前聊天和账号保留本地草稿，正常重启后也会恢复。
   详见[输入与回复](UX.md)。
5. `G` 回到最新消息，`?` 打开帮助；导航状态按 `q` 退出。

二维码会自动轮换。Tab 切换紧凑字符块与较大的全单元格显示，Esc 返回手机号登录。
F2 切换已有账号，F3 添加账号；登录页和错误页也能使用。
以上均为默认按键，可通过 [Lua 配置](Configuration.md) 修改。

## 使用 Cargo 从源码安装

Linux x86_64/ARM64、macOS Intel/Apple silicon、Windows x64/ARM64 使用同一条 Cargo
命令。Cargo 按仓库锁文件编译选定的 Git 源码，使用 release profile 安装 `tg`，
Windows 下为 `tg.exe`。

### 首次准备构建工具

通过 [rustup](https://rustup.rs/) 安装 Rust，并准备 Git 和平台的 C 编译器、链接器。
Lua 与 SQLite 随应用编译，无需单独安装它们的系统包。

| 平台 | 原生构建工具 |
| --- | --- |
| Linux | C 编译器、链接器、make 和 Git；Debian/Ubuntu 可执行 `sudo apt-get install build-essential git` |
| macOS | Xcode Command Line Tools：`xcode-select --install` |
| Windows | Visual Studio C++ 构建工具和 Windows SDK，选择对应 CPU 的 MSVC 工具；ARM64 需要 ARM64 组件。参阅 [rustup MSVC 配置](https://rust-lang.github.io/rustup/installation/windows-msvc.html)。 |

安装项目固定的编译器，不改变系统默认 Rust 工具链：

```sh
rustup toolchain install 1.98.0 --profile minimal
```

### 一条命令安装或升级

以下命令可直接用于 Bash、Zsh 和 PowerShell：

```sh
cargo +1.98.0 install --locked --git https://github.com/iebb/termgram --bin tg termgram
```

它安装仓库默认分支；再次执行即可升级到该分支的最新源码。`+1.98.0` 明确选择编译器，
在仓库外执行也有效。下载后的仓库工具链文件不会替换已经启动的 Cargo 进程。

默认安装位置为 Linux/macOS 的 `~/.cargo/bin/tg`，Windows 的
`%USERPROFILE%\.cargo\bin\tg.exe`，需要把该目录加入 PATH。自定义 `CARGO_HOME` 或
Cargo 安装根目录设置会改变位置；`--root DIR` 安装到 `DIR/bin`，参数应为 bin 的父目录。
若旧版 `tg` 优先被找到，用 Bash/Zsh 的 `type -a tg` 或 PowerShell 的
`Get-Command tg -All` 查看，并移除 PATH 中过期的可执行文件。

安装其他分支时添加 `--branch BRANCH`；固定提交则改用 `--rev COMMIT`，把占位文字替换
为所需 Git 引用。PR 功能在合并前需要选择该 PR 的源码分支。此安装方式需要保留 Git URL；
不要改成 `cargo install termgram`，后者会查找 crates.io 上的包。

若要安装与已发布 Wiki 完全一致的版本，打开页尾的 **文档来源**，取得对应提交，
再用 `--rev COMMIT` 安装。这样可以避免阅读尚在评审中的新功能文档时，实际安装了默认分支。

`cargo uninstall termgram` 卸载 Cargo 管理的程序，保留 Termgram 配置与账号数据。
使用过 `--root` 时，更新和卸载也应提供相同目录。`--force` 可以重编同一提交，例如修改了
内置应用凭据后；`--target-dir DIR` 可以保留构建产物，供后续源码更新复用。

更多选项与安装记录规则见 [Cargo 安装文档](https://doc.rust-lang.org/cargo/commands/cargo-install.html)。
Cargo 源码升级与 `tg update` 的区别见[更新](Updates.md)。

### Telegram 应用凭据

Git 源码安装需要在 [my.telegram.org/apps](https://my.telegram.org/apps) 创建自己的应用，
获取 API ID 与 API hash。它们标识客户端；启动 `tg` 后仍需登录 Telegram 账号。
源码安装不会获得项目 GitHub Actions 的发行版凭据。

启动前设置运行时环境变量。Bash/Zsh：

```sh
export TELEGRAM_API_ID=123456
export TELEGRAM_API_HASH=your_api_hash
tg
```

PowerShell：

```powershell
$env:TELEGRAM_API_ID = '123456'
$env:TELEGRAM_API_HASH = 'your_api_hash'
tg
```

请替换示例值，运行时变量优先于程序内置默认值。应用也会读取工作目录或其父目录中的
`.env`。若需要一个不依赖运行时变量的个人可执行文件，可在构建环境中设置
`TERMGRAM_EMBEDDED_API_ID` 与 `TERMGRAM_EMBEDDED_API_HASH` 后执行 Cargo；这些应用
凭据会成为可执行文件的一部分，账号会话不会被内置。

### 安装本地 checkout

开发时克隆仓库，可把 `.env.example` 复制为 `.env` 设置运行时凭据，再安装当前源码：

```sh
git clone https://github.com/iebb/termgram.git
cd termgram
cargo install --locked --path . --bin tg
```

此时使用仓库固定的工具链。`cargo run --locked --release` 可以直接运行而不安装。
不要提交 `.env` 或会话数据库。

路径与环境变量见[配置](Configuration.md)，图片/按键问题见[终端集成](Terminal.md)，
离线与缓存行为见[缓存与同步](Synchronization.md)。
