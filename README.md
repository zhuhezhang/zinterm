# ZinTerm

**简体中文** | [English](./README.en.md)

ZinTerm 是一款基于 Rust + Slint 开发的轻量级、低内存跨平台终端模拟器，支持 SSH、SFTP、Telnet 协议、串口连接（Serial）、本地 Shell。它把内存占用从 electron 的 200 MB+  压到几十 MB 的原生级别。

## 截图

<p align="center">
  <img src="docs/screenshots/terminal.png" width="800"><br>
</p>

<p align="center">
  <img src="docs/screenshots/connection.png" width="800"><br>
</p>

<p align="center">
  <img src="docs/screenshots/settings.png" width="800"><br>
</p>

## 下载与安装

每次打 `v*` 标签，GitHub Actions 会自动构建 **Windows / Linux / macOS** 三平台二进制，
发布到 [Releases](https://github.com/zhuhezhang/zinterm/releases) 页面。

### Windows

下载 `zinterm-*-windows-x86_64.zip`，解压后双击 `zinterm.exe`。

### Linux

```bash
tar -xzf zinterm-*-linux-x86_64.tar.gz
cd zinterm-*-linux-x86_64
./zinterm                                  # 直接运行
# 可选：装应用图标 + 启动器入口（Dock / 应用列表里显示图标，无需传参）
chmod +x install-linux.sh && ./install-linux.sh
```

> 需要 glibc ≥ 2.35（Ubuntu 22.04+ / Debian 12+）。Wayland 下首次装完图标可能要注销重登一次。

从源码 `cargo run`（Linux Mint / Ubuntu / Debian）需要先安装 Slint/winit/rfd 等用到的系统开发包：

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

### macOS

下载得到的是 `.zip`，里面是 `zinterm.app` 应用程序包：

```bash
# 解压(aarch64 = Apple 芯片，x86_64 = Intel)
unzip zinterm-*-macos-*.zip
# 移到「应用程序」(可选，留在原地也行)
mv zinterm.app /Applications/
# 去掉「未签名应用」的隔离属性，否则会提示「zinterm 已损坏，无法打开」
xattr -dr com.apple.quarantine /Applications/zinterm.app
# 打开(或在「访达」里双击)
open /Applications/zinterm.app
```

> 若未移到 `/Applications`，把上面两条路径换成 `.app` 实际所在位置(如 `~/Downloads/zinterm.app`)即可。

> 从源码构建见下方 [运行](#运行)。

## 功能

### 已实现

- [x] FinalShell 风格 UI，深色 / 浅色 / 跟随系统主题
- [x] 完整 VT/ANSI 终端模拟（btop / htop / vim 全屏正常渲染）
- [x] 彩色 emoji（支持肤色、旗帜及 ZWJ 组合序列）
- [x] 多标签页（欢迎页 + 多个会话）
- [x] 会话管理：新建 / 编辑 / 删除 / 分组，本地 JSON 持久化，导出 / 导入
  - 配置目录：`%APPDATA%/zinterm`（Windows）
    / `~/.config/zinterm`（Linux）
    / `~/Library/Application Support/zinterm`（macOS）
  - `sessions.json`：会话与空分组（`empty_groups`）；`commands.json`：快捷命令、空快捷分组（`quick_empty_groups`）与命令历史
  - 导出文件不含密码 / 私钥；若需导入凭据，见下方 [导入连接与密码字段](#导入连接与密码字段)
- [x] SSH（`russh`，纯 Rust）：密码 / 私钥 / 加密私钥（密码短语）
- [x] SFTP 文件浏览 + 上传 / 下载（拖拽）+ 终端内 ZMODEM（`sz`）接收
- [x] 快捷命令 + 命令输入框（可群发到所有会话）+ 命令历史
- [x] 串口 / Telnet 会话
- [x] 会话密码加密存储（ChaCha20-Poly1305 + OS 钥匙串主密钥，`zinterm-credentials-vault.json`）
- [x] 已知主机（`zinterm-known-hosts.json` 指纹库）校验 + 首次连接确认
- [x] 多标签页终端分屏

彩色 emoji 图形来自 [Twemoji](https://github.com/jdecked/twemoji)，按
[CC BY 4.0](https://creativecommons.org/licenses/by/4.0/) 使用；完整署名见
[THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md)。


## 技术栈

| 模块          | 选型                                                              |
| ------------- | ----------------------------------------------------------------- |
| UI            | [Slint](https://slint.dev)（纯 Rust 编译，无 GC）                 |
| 异步运行时    | [`tokio`](https://tokio.rs)                                       |
| SSH 协议      | [`russh`](https://crates.io/crates/russh)（无 libssh 依赖）       |
| 序列化        | `serde` + `serde_json`                                            |
| 日志          | `tracing` + `tracing-subscriber`                                  |

## 运行

```bash
cargo run --release
```

首次启动会在配置目录建立空的 `sessions.json` / `commands.json` 等文件。点击右上
角 **“＋ 新建会话”** 添加第一台服务器。

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
├── ui/
│   ├── app.slint            # 顶层窗口（唯一根文件）
│   ├── shared/              # theme / widgets
│   ├── shell/               # 标题栏、标签栏、停靠区等
│   ├── terminal/            # 终端视图、SFTP、命令栏
│   ├── welcome/             # 欢迎页 / 会话列表
│   ├── dialogs/             # 各类弹框
│   ├── interface/           # 设置面板
│   ├── types/               # 共享数据结构
│   └── fonts/
└── src/
    ├── main.rs
    ├── app.rs               # UI ↔ 后端桥接
    ├── config.rs            # 会话 JSON 持久化
    └── ssh.rs               # SSH 会话 worker
```

## 开发提示

- Slint 控件有非常严格的布局 DSL，改 `.slint` 后 `cargo check` 是最快的
  反馈方式。
- 应用事件循环是单线程（Slint 要求），所有跨线程 UI 更新通过
  `slint::invoke_from_event_loop` 回调。
- SSH / SFTP 共享已知主机校验：指纹存于 `zinterm-known-hosts.json`，首次连接会确认并记住，
  后续密钥变化会再次提示。

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

## License

MIT。详见 [LICENSE](LICENSE)。
