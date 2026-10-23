# 安装指南

本指南覆盖运行依赖和所有受支持的安装方式。项目介绍与日常使用请从
[README](../README.zh-CN.md) 开始。

## 环境要求

- Linux 或 macOS；Linux 为主要目标平台。
- `mpv`。
- `ffmpeg` 与 `ffprobe`。
- 仅从源码构建时需要 Rust 1.88 或更新版本。

SQLite 已编入可执行文件，不需要额外安装 SQLite 命令行工具。可用包管理器
安装媒体工具，例如：

```sh
# Debian / Ubuntu
sudo apt install mpv ffmpeg

# macOS
brew install mpv ffmpeg
```

## 从 npm 安装

```sh
npm install -g @hmgle/radio-cli
```

此方式安装对应平台的预编译 `radio` 二进制（Linux x86_64、macOS Intel 或
Apple Silicon），需要 Node.js 18 或更新版本。运行时仍需要 `mpv`、`ffmpeg`
和 `ffprobe`。

## 安装预编译发行包

[发布页面](https://github.com/hmgle/radio-cli/releases)提供发行归档、英文
shell 补全、手册页和统一的 `SHA256SUMS`。使用发行包不需要 Rust 工具链。

| 平台 | 发布目标 | 状态 |
| --- | --- | --- |
| Linux x86_64 | `x86_64-unknown-linux-musl` | 主要支持；静态链接 |
| macOS Apple Silicon | `aarch64-apple-darwin` | 实验性 |
| macOS Intel | `x86_64-apple-darwin` | 实验性 |

下载 `SHA256SUMS` 和对应平台的归档，然后在下载目录中校验、解压并运行。
把下面命令中的 `x86_64-unknown-linux-musl` 换成上表中你的平台目标；归档名
里的 `*` 对应版本号，因此同一套命令适用于任何版本：

```sh
# 校验下载文件与发布的校验和一致。
grep -F 'x86_64-unknown-linux-musl' SHA256SUMS | sha256sum -c -
# macOS 上：grep -F 'aarch64-apple-darwin' SHA256SUMS | shasum -a 256 -c -

# 解压归档并启动 radio。
tar -xzf radio-cli-v*-x86_64-unknown-linux-musl.tar.gz
cd radio-cli-v*-x86_64-unknown-linux-musl
./radio doctor
./radio sync
./radio tui
```

macOS 发行包未经 Developer ID 签名或公证。若 Gatekeeper 阻止运行你信任的
下载文件，请先校验校验和，再对解压出的程序执行
`xattr -d com.apple.quarantine ./radio`。参见
[Apple 官方说明](https://support.apple.com/en-us/102445)。

## 从 crates.io 安装

```sh
cargo install radio-cli --locked
```

`cargo install` 会从源码构建，因此需要 Rust 1.88 或更新版本的工具链，
并把 `radio` 安装到 `~/.cargo/bin`。运行时仍需要 `mpv`、`ffmpeg` 和
`ffprobe`。

## 从源码构建

```sh
git clone https://github.com/hmgle/radio-cli.git
cd radio-cli
cargo build --release --locked
./target/release/radio doctor
./target/release/radio sync
./target/release/radio tui

# 或安装到 ~/.cargo/bin
cargo install --path . --locked
```

## 首次运行

当输入和输出都是交互终端时，直接运行 `radio` 会打开 TUI；任一输入或输出被
重定向时，会改为打印帮助。`radio tui` 始终请求启动 TUI。

本地没有目录时，TUI 和 `radio random` 会先获取最热门的 1,000 个电台。运行
`radio sync` 可缓存完整目录。同步失败会保留原目录。`radio doctor` 检查
mpv、FFmpeg、ffprobe、录音目录和剩余空间。
