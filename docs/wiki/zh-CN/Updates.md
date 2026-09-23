# 更新与发行

[English](../en/Updates.md) · [指南首页](Home.md)

`tg update` 使用所选通道，`tg update --stable` 选择稳定版，
`tg update --prerelease` 包含预发布版。稳定通道选最高的稳定 `0.1.Z`；预发布通道在
两类发行中选最高的 `0.1.Z`，因此不会用较旧预发布版覆盖更新的稳定版。

后台对所选通道每天最多自动检查一次，在底部轻量提示。实际安装必须执行更新命令。
`s` 设置中可以关闭检查或更改通道。更新器通过 GitHub HTTPS 获取并验证 `SHA256SUMS`，
再替换可执行文件。Windows 在更新进程退出后完成替换；其他进程可能占用程序，
再次执行 `tg update` 会重新下载并校验。

`tg --version` 显示版本、提交、分支、系统、架构和构建号。提交号后的 `*` 表示源码树
有非忽略的改动。重定向时输出纯文本；`NO_COLOR=1` 或 `TERM=dumb` 也能关闭版本信息着色。
没有 Git 元数据的源码包对缺失字段显示 `unknown`。

## Cargo 源码安装的更新

再次执行 [Cargo 安装命令](Getting-Started.md) 即可从最新源码重编；使用过
`--branch`、`--rev` 或 `--root` 时保留相应选项。固定 `--rev` 会一直停留在该提交，
需要主动改为其他提交才能更新。即使 manifest 的包版本仍是 `0.1.0`，Cargo 也会记录 Git 来源。

Git 源码构建按 first-parent 提交高度显示 `0.1.Z`，通过显示的提交号确认源码；Cargo 的
checkout 可能显示本地的 `master` 或 `HEAD`，不一定是指定的源码分支。没有 Git 元数据的源码包回退到 manifest
版本，CI 可以指定版本覆盖值。

`tg update` 下载 GitHub 上预编译的发行版，不会执行 Cargo 或跟随源码分支。
要继续跟随源码更新，请使用 Cargo 命令。

## 维护者发行流程

CI 检查格式、严格 Clippy、测试、安装器，并构建六个原生包：Linux x86_64/ARM64、
macOS Intel/Apple silicon、Windows x64/ARM64。默认分支构建从 Actions 的
`TELEGRAM_API_ID`、`TELEGRAM_API_HASH` secrets 获取应用凭据，PR 构建不会获取它们。

每个成功的默认分支提交发布为 `0.1.Z`，`Z` 是 first-parent 提交高度。
普通提交为预发布版；包元数据提交（标题以 `chore(release):` 开头）不会发布，
它们只把仓库内的 Homebrew 和 Scoop 文件指向最新的稳定版与预发布版。
标题以 `release:` 开头则发布稳定版，例如：

```sh
git commit --allow-empty -m "release: stable"
git push
```

发布复用通过验证的构建产物，包含安装器与 `SHA256SUMS`。检查失败不会发布，中断的发布
在后续推送时重试，不移动已有标签。自动发布从高度 2 开始。
旧资产名 `linux`、`macos`、`windows` 分别表示 Linux x86_64、Apple-silicon macOS、
Windows x64；其他架构使用明确后缀。

编译进程序的应用 API 凭据可以被提取，它们不是用户的 Telegram 会话。
用户手机号、验证码、密码、会话文件不进入 CI。Termgram 是非官方项目，与 Telegram 无隶属关系。
