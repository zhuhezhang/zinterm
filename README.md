# ZinTerm

**简体中文** | [English](./README.en.md)

ZinTerm 是一款基于 **Rust + [Slint](https://slint.dev)** 开发的轻量级、低内存跨平台终端模拟器。
它面向日常运维与远程开发场景，把 Electron 类终端动辄 **200 MB+** 的内存占用压到原生级别的几十 MB，
同时保留多协议连接、会话管理、文件传输与快捷命令等实用能力。本项目 fork [meatshell](https://github.com/yituorou/meatshell/) 的 v0.6.12 版本，并根据本人使用 MobaXterm 的习惯、日常使用场景进行二次开发，感谢 yituorou 的开源！

支持的连接类型：

- **SSH** — 密码 / OpenSSH 私钥 / 加密私钥（口令）/ PuTTY PPK
- **SFTP** — 文件浏览、上传下载、拖拽、终端内 ZMODEM（`sz`）接收
- **Telnet** — 传统设备与实验室环境
- **串口（Serial）** — COM / ttyUSB 等本地串口
- **本地 Shell** — 本机终端会话

目标平台：Windows / Linux / macOS。每次打 `v*` 标签都会由 GitHub Actions 自动构建三平台二进制。

## 截图

<p align="center">
  <img src="docs/screenshots/terminal.png" width="800"><br>
  <em>终端会话 · 多标签 · 命令栏</em>
</p>

<p align="center">
  <img src="docs/screenshots/connection.png" width="800"><br>
  <em>新建 / 编辑连接</em>
</p>

<p align="center">
  <img src="docs/screenshots/settings.png" width="800"><br>
  <em>设置面板</em>
</p>

## 为什么选 ZinTerm

| 关注点 | ZinTerm |
| ------ | ------- |
| 内存 | 原生 Rust UI，常见场景几十 MB，而非 Electron 的 200 MB+ |
| UI | FinalShell 风格布局，深色 / 浅色 / 跟随系统 |
| 协议 | SSH · SFTP · Telnet · 串口 · 本地 Shell 同窗口管理 |
| 安全 | 凭据加密入库 + 系统钥匙串主密钥；已知主机指纹校验 |
| 构建 | 纯 Rust 依赖链（SSH 用 `russh`，无 libssh / OpenSSL 硬依赖） |
| 语言 | 界面中英双语，可跟随系统或手动切换 |
| 快捷键 | 全局 `Ctrl+F` 聚焦会话搜索框，↑/↓ 选择后回车连接，操作更顺畅。更多快捷键可查看参考设置→快捷键 |

## 下载与安装

每次推送 `v*` 标签，GitHub Actions 会自动构建 **Windows / Linux / macOS** 三平台二进制，
发布到 [Releases](https://github.com/zhuhezhang/zinterm/releases) 页面。
也可在应用内 **设置 → 更新** 检查新版本（后台请求 GitHub Releases API）。

### Windows

1. 下载 `zinterm-*-windows-x86_64.zip`
2. 解压到任意目录
3. 双击 `zinterm.exe` 即可运行（资源管理器 / 任务栏图标已嵌入 exe）

无需安装程序；需要时可自行创建快捷方式或固定到任务栏。

### Linux

```bash
tar -xzf zinterm-*-linux-x86_64.tar.gz
cd zinterm-*-linux-x86_64
./zinterm                                  # 直接运行
# 可选：装应用图标 + 启动器入口（Dock / 应用列表里显示图标，无需传参）
chmod +x install-linux.sh && ./install-linux.sh
```

> 需要 glibc ≥ 2.35（Ubuntu 22.04+ / Debian 12+）。Wayland 下首次装完图标可能要注销重登一次。
> Wayland 剪贴板走 `wlr-data-control`，不依赖 XWayland。

从源码 `cargo run`（Linux Mint / Ubuntu / Debian）需要先安装 Slint / winit / rfd 等用到的系统开发包：

```bash
sudo apt update
sudo apt install -y --no-install-recommends \
  build-essential pkg-config cmake \
  libfontconfig1-dev libfreetype6-dev \
  libxcb1-dev libxcb-render0-dev libxcb-shape0-dev libxcb-xfixes0-dev \
  libxkbcommon-dev libxkbcommon-x11-dev libwayland-dev \
  libgl1-mesa-dev libegl1-mesa-dev libgtk-3-dev \
  libudev-dev
```

`libudev-dev` 用于串口枚举；`libgtk-3-dev` 用于原生文件对话框（rfd）。

### macOS

下载得到的是 `.zip`，里面是 `zinterm.app` 应用程序包（`aarch64` = Apple 芯片，`x86_64` = Intel）：

```bash
# 解压
unzip zinterm-*-macos-*.zip
# 移到「应用程序」(可选，留在原地也行)
mv zinterm.app /Applications/
# 去掉「未签名应用」的隔离属性，否则会提示「zinterm 已损坏，无法打开」
xattr -dr com.apple.quarantine /Applications/zinterm.app
# 打开(或在「访达」里双击)
open /Applications/zinterm.app
```

> 若未移到 `/Applications`，把上面两条路径换成 `.app` 实际所在位置（如 `~/Downloads/zinterm.app`）即可。
> 从源码构建见下方 [从源码运行](#从源码运行)。macOS 可选 Skia 渲染器：`SLINT_BACKEND=winit-skia`。

## 功能

### 已实现

-  FinalShell 风格 UI；深色 / 浅色 / 跟随系统主题；可选自定义壁纸
-  完整 VT/ANSI 终端模拟（btop / htop / vim 等全屏程序正常渲染）
-  彩色 emoji（肤色、旗帜及 ZWJ 组合序列；嵌入 Twemoji PNG）
-  多标签页（欢迎页 + 多个会话）；终端分屏（水平 / 垂直）
-  会话管理：新建 / 编辑 / 删除 / 分组；欢迎页搜索（全局 `Ctrl+F`）；本地 JSON 持久化
-  连接 / 快捷命令 / 设置均可导入与导出
-  SSH（`russh`，纯 Rust）：密码 / 私钥 / 加密私钥（口令）/ PuTTY PPK
-  可选更多 SSH 算法；可配置 SSH 保活间隔（兼容老旧设备）
-  SFTP 文件浏览 + 上传 / 下载（拖拽）+ 终端内 ZMODEM（`sz`）接收
-  快捷命令 + 命令输入框（可群发到所有会话）+ 命令历史 + 快捷命令分组
-  串口 / Telnet / 本地 Shell 会话
-  会话密码加密存储（ChaCha20-Poly1305 + OS 钥匙串主密钥）；可选是否保存密码 / 密钥
-  已知主机指纹校验 + 首次连接确认；可选是否保存主机密钥
-  数据清理：清除已保存密码/密钥、分组与会话、主机密钥；一键恢复默认设置
-  终端查找、输出高亮规则、字体 / 编码 / 输入相关设置
-  界面语言：跟随系统 / 简体中文 / English（运行时切换）
-  应用内检查更新（GitHub Releases）

### 配置与数据文件

配置目录：

| 平台 | 路径 |
| ---- | ---- |
| Windows | `%APPDATA%\zinterm` |
| Linux | `~/.config/zinterm` |
| macOS | `~/Library/Application Support/zinterm` |

主要文件：

| 文件 | 用途 |
| ---- | ---- |
| `sessions.json` | 会话与空分组（`empty_groups`） |
| `commands.json` | 快捷命令、空快捷分组（`quick_empty_groups`）、命令历史 |
| `zinterm-credentials-vault.json` | 加密后的密码 / 密钥材料（主密钥在系统钥匙串） |
| `zinterm-known-hosts.json` | SSH / SFTP 主机指纹库 |

导出连接文件**不含**密码 / 私钥；若需带凭据迁移，见
[导入连接与密码字段](#导入连接与密码字段)。

彩色 emoji 图形来自 [Twemoji](https://github.com/jdecked/twemoji)，按
[CC BY 4.0](https://creativecommons.org/licenses/by/4.0/) 使用；完整署名见
[THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md)。

## 技术栈

| 模块 | 选型 |
| ---- | ---- |
| UI | [Slint](https://slint.dev)（编译为纯 Rust，无 GC） |
| 异步运行时 | [`tokio`](https://tokio.rs) |
| SSH | [`russh`](https://crates.io/crates/russh)（vendored；无 libssh） |
| SFTP | `russh-sftp` |
| 终端仿真 | `vt100` + 自研渲染 / 查找 / 高亮 |
| 本地 PTY | `portable-pty` |
| 串口 | `serialport` |
| 凭据 | ChaCha20-Poly1305 + `keyring`（系统钥匙串） |
| 序列化 | `serde` + `serde_json` |
| 日志 | `tracing` + `tracing-subscriber` |
| 国际化 | 内置 gettext 风格文案（`lang/`） |

## 从源码运行

要求：Rust **1.75+**（见 `Cargo.toml` 的 `rust-version`），以及上方 Linux 系统依赖（若适用）。

```bash
git clone https://github.com/zhuhezhang/zinterm.git
cd zinterm
cargo run --release
```

首次启动会在配置目录建立空的 `sessions.json` / `commands.json` 等文件。
点击右上角 **「＋ 新建会话」** 添加第一台服务器，或在欢迎页从已有分组打开会话。

常用调试：

```bash
# 开发构建（更快编译，运行更慢）
cargo run

# 仅类型检查（改 .slint 后最快反馈）
cargo check

# 查看版本
cargo run --release -- --version
```

## 导入连接与密码字段

设置菜单中的 **导出连接** 会写出可移植 JSON（`zinterm_export: "sessions"`），
但**不会**写入 `password`、`key_passphrase`、`private_key`。
需要带凭据迁移时，可在导入前用文本编辑器手工补上这些字段，再用
**导入连接** 读入。

是否把导入的密码 / 密钥落到本地，取决于 **设置 → 数据 → 保存密码/密钥**：

- 开关**打开**：按认证方式写入对应字段，并保存到加密凭证库
  （`zinterm-credentials-vault.json`，主密钥在系统钥匙串）。
- 开关**关闭**：上述字段即使写在文件里也会被忽略，连接仍可导入，
  首次连接时再输入凭据。

### 字段说明（仅 SSH）

| 字段 | 含义 |
| ---- | ---- |
| `auth` | `"password"`（默认）或 `"key"` |
| `password` | **仅密码认证**：登录密码 |
| `key_passphrase` | **仅密钥认证**：加密私钥的口令（可空） |
| `private_key` | **仅密钥认证**：本机私钥路径，或粘贴的私钥正文（程序按内容自动区分） |

多行私钥请写在**一行 JSON 字符串**里，行与行之间用 `\n` 分隔，例如：

```json
{
  "zinterm_export": "sessions",
  "version": 1,
  "exported_at": "manual",
  "empty_groups": [],
  "sessions": [
    {
      "kind": "ssh",
      "name": "lab",
      "host": "192.168.1.10",
      "port": 22,
      "user": "root",
      "auth": "password",
      "password": "your-login-password"
    },
    {
      "kind": "ssh",
      "name": "key-host",
      "host": "192.168.1.11",
      "user": "ubuntu",
      "auth": "key",
      "key_passphrase": "optional-key-passphrase",
      "private_key": "-----BEGIN OPENSSH PRIVATE KEY-----\nAAAA...\n-----END OPENSSH PRIVATE KEY-----"
    },
    {
      "kind": "ssh",
      "name": "key-path",
      "host": "192.168.1.12",
      "user": "ubuntu",
      "auth": "key",
      "private_key": "/home/ubuntu/.ssh/id_ed25519"
    }
  ]
}
```

> 导入文件中的明文密码仅用于一次性迁移；开启「保存密码」后会写入本机加密凭证库
> （ChaCha20-Poly1305，主密钥在系统钥匙串）。请勿把含明文密码的 JSON 提交到公开仓库。

## 项目布局

```
zinterm/
├── Cargo.toml
├── build.rs                 # Slint 编译器入口
├── lang/                    # 中英文文案（gettext .po / .mo）
├── scripts/                 # 发版脚本等
├── ui/
│   ├── app.slint            # 顶层窗口（唯一根文件）
│   ├── shared/              # theme / widgets
│   ├── shell/               # 标题栏、标签栏、停靠区等
│   ├── terminal/            # 终端视图、SFTP、命令栏、查找
│   ├── welcome/             # 欢迎页 / 会话列表
│   ├── dialogs/             # 各类弹框
│   ├── interface/           # 设置面板各页
│   ├── types/               # 共享数据结构
│   └── fonts/
└── src/
    ├── main.rs
    ├── app.rs / app/        # UI ↔ 后端桥接、回调、分屏
    ├── config/              # 会话 / 偏好 / 凭据库持久化
    ├── ssh/                 # SSH 会话、认证、已知主机、PPK
    ├── sftp/                # SFTP 浏览与传输
    ├── terminal/            # PTY、Telnet、串口、渲染、ZMODEM
    ├── layout/              # 分屏布局树
    ├── i18n/                # 运行时语言切换
    ├── wallpaper/           # 自定义壁纸
    └── logging/             # tracing 初始化与错误日志
```

## 开发提示

- Slint 控件有非常严格的布局 DSL，改 `.slint` 后 `cargo check` 是最快的反馈方式。
- 应用事件循环是单线程（Slint 要求），所有跨线程 UI 更新通过
  `slint::invoke_from_event_loop` 回调。
- SSH / SFTP 共享已知主机校验：指纹存于 `zinterm-known-hosts.json`，首次连接会确认并记住，
  后续密钥变化会再次提示。
- `russh` 以 `vendor/russh` 形式 vendored（含兼容补丁，例如部分设备的 banner / MAC）；
  改 SSH 行为时先看该目录与 `Cargo.toml` 中的注释。
- 发布前请走下方发版脚本，保证 tag 与 `Cargo.toml` 版本一致。

## 发版

使用发布脚本，让 Git tag 指向的提交本身就已经包含匹配的 Cargo 版本号。
远程名通过 `-Remote` / `--remote` 传入（不一定是 `origin`）。

Windows（PowerShell）：

```powershell
.\scripts\release.ps1 v0.6.0 -Push -Remote zinterm_github
```

macOS / Linux：

```bash
./scripts/release.sh v0.6.0 --push --remote zinterm_github
```

脚本会：

- 要求已跟踪文件没有未提交改动
- 更新 `Cargo.toml` 和 `Cargo.lock` 里的 `zinterm` 版本号
- 运行 `cargo check --locked`
- 验证 `zinterm --version` 输出匹配 tag
- 提交版本号变更
- 创建 annotated tag
- 传入 `-Push` / `--push` 时推送到指定远程的当前分支和 tag

如果想先在本地创建提交和 tag，不立即推送：

```powershell
# Windows
.\scripts\release.ps1 v0.6.0 -Remote zinterm_github
git push zinterm_github HEAD
git push zinterm_github v0.6.0
```

```bash
# macOS / Linux
./scripts/release.sh v0.6.0 --remote zinterm_github
git push zinterm_github HEAD
git push zinterm_github v0.6.0
```

Release workflow 也会检查推送上来的 tag。比如 tag 名是 `v0.6.0` 时，
`Cargo.toml`、`Cargo.lock` 和构建出的 `zinterm --version` 都必须是
`0.6.0`，否则 workflow 会在发布前失败。

镜像仓库（可选）：`https://gitee.com/zhuhezhang/zinterm`。

## License

MIT。详见 [LICENSE](LICENSE)。
