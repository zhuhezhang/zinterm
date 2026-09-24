# ZinTerm

[简体中文](./README.md) | **English**

ZinTerm is a lightweight, low-memory cross-platform terminal emulator built with **Rust + [Slint](https://slint.dev)**.
It targets day-to-day ops and remote development, cutting Electron-class terminals from **200+ MB** of RAM
down to tens of MB at native performance, while keeping multi-protocol sessions, file transfer, and quick commands. This project is a fork of [meatshell](https://github.com/yituorou/meatshell/) v0.6.12, further developed around the author's MobaXterm habits and day-to-day workflows. Thanks to yituorou for the open-source work!

Supported connection types:

- **SSH** — password / OpenSSH private key / encrypted key (passphrase) / PuTTY PPK
- **SFTP** — file browser, upload/download, drag-and-drop, in-terminal ZMODEM (`sz`) receive
- **Telnet** — legacy gear and lab environments
- **Serial** — local ports such as COM / ttyUSB
- **Local shell** — a native shell session on this machine

Target platforms: Windows / Linux / macOS. Every `v*` tag is built for all three by GitHub Actions.

## Screenshots

<p align="center">
  <img src="docs/screenshots/terminal-en.png" width="800"><br>
  <em>Terminal session · tabs · command bar</em>
</p>

<p align="center">
  <img src="docs/screenshots/connection-en.png" width="800"><br>
  <em>Create / edit a connection</em>
</p>

<p align="center">
  <img src="docs/screenshots/settings-en.png" width="800"><br>
  <em>Settings panel</em>
</p>

## Why ZinTerm

| Concern | ZinTerm |
| ------- | ------- |
| Memory | Native Rust UI — tens of MB in common use, not Electron’s 200+ MB |
| UI | FinalShell-style layout; dark / light / follow system |
| Protocols | SSH · SFTP · Telnet · serial · local shell in one window |
| Security | Encrypted credentials vault + OS keyring master key; known-hosts checks |
| Build | Pure Rust stack (SSH via `russh`; no hard libssh / OpenSSL dependency) |
| Language | Chinese / English UI; follow system or pick manually |
| Shortcuts | Global `Ctrl+F` focuses the session search box; ↑/↓ then Enter to connect. For more shortcuts, see Settings → Shortcuts |

## Download & install

Every `v*` tag triggers a GitHub Actions build for **Windows / Linux / macOS**,
published on the [Releases](https://github.com/zhuhezhang/zinterm/releases) page.
You can also use **Settings → Update** inside the app (background GitHub Releases API check).

### Windows

1. Download `zinterm-*-windows-x86_64.zip`
2. Unzip to any folder
3. Run `zinterm.exe` (Explorer / taskbar icon is embedded in the exe)

No installer is required; optionally create a shortcut or pin it to the taskbar.

### Linux

```bash
tar -xzf zinterm-*-linux-x86_64.tar.gz
cd zinterm-*-linux-x86_64
./zinterm                                  # run it directly
# Optional: install the app icon + launcher entry (dock / app list; no args needed)
chmod +x install-linux.sh && ./install-linux.sh
```

> Requires glibc ≥ 2.35 (Ubuntu 22.04+ / Debian 12+). On Wayland you may need to log out/in once after installing the icon.
> Wayland clipboard uses `wlr-data-control` and does not depend on XWayland.

Building from source with `cargo run` on Linux Mint / Ubuntu / Debian requires the Slint / winit / rfd system packages:

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

`libudev-dev` is for serial-port enumeration; `libgtk-3-dev` is for native file dialogs (rfd).

### macOS

The download is a `.zip` containing the `zinterm.app` bundle (`aarch64` = Apple Silicon, `x86_64` = Intel):

```bash
# Unzip
unzip zinterm-*-macos-*.zip
# Move it to Applications (optional — it also runs in place)
mv zinterm.app /Applications/
# Clear the quarantine flag, otherwise macOS says "zinterm is damaged and can't be opened"
xattr -dr com.apple.quarantine /Applications/zinterm.app
# Open it (or double-click in Finder)
open /Applications/zinterm.app
```

> If you didn't move it to `/Applications`, point both paths above at wherever the `.app` actually is (e.g. `~/Downloads/zinterm.app`).
> To build from source, see [Running from source](#running-from-source). Optional Skia renderer on macOS: `SLINT_BACKEND=winit-skia`.

## Features

### Done

-  FinalShell-style UI; dark / light / follow-system themes; optional custom wallpaper
-  Full VT/ANSI terminal emulation (btop / htop / vim and other full-screen apps render correctly)
-  Color emoji (skin tones, flags, ZWJ sequences; embedded Twemoji PNGs)
-  Tabs (welcome page + multiple sessions); terminal split panes (horizontal / vertical)
-  Session management: create / edit / delete / groups; welcome search (global `Ctrl+F`); local JSON
-  Import / export for connections, quick commands, and settings
-  SSH (`russh`, pure Rust): password / private key / encrypted key (passphrase) / PuTTY PPK
-  Optional extra SSH algorithms; configurable SSH keepalive interval (older gear friendly)
-  SFTP browser + upload / download (drag-and-drop) + in-terminal ZMODEM (`sz`) receive
-  Quick commands + command box (broadcast to all sessions) + history + quick-command groups
-  Serial / Telnet / local shell sessions
-  Session passwords encrypted at rest (ChaCha20-Poly1305 + OS keyring); optional save passwords / keys
-  Known-hosts fingerprint check + first-connect confirmation; optional save host keys
-  Data cleanup: clear saved passwords/keys, groups & sessions, host keys; restore default settings
-  Terminal find, output-highlight rules, font / encoding / input-related settings
-  UI language: follow system / 简体中文 / English (switchable at runtime)
-  In-app update check (GitHub Releases)

### Config & data files

Config directory:

| Platform | Path |
| -------- | ---- |
| Windows | `%APPDATA%\zinterm` |
| Linux | `~/.config/zinterm` |
| macOS | `~/Library/Application Support/zinterm` |

Main files:

| File | Purpose |
| ---- | ------- |
| `sessions.json` | Sessions and empty groups (`empty_groups`) |
| `commands.json` | Quick commands, empty quick groups (`quick_empty_groups`), history |
| `zinterm-credentials-vault.json` | Encrypted password / key material (master key in the OS keyring) |
| `zinterm-known-hosts.json` | SSH / SFTP host fingerprint store |

Exported connection files **omit** passwords / private keys; to migrate credentials see
[Importing connections and password fields](#importing-connections-and-password-fields).

Color emoji graphics are provided by [Twemoji](https://github.com/jdecked/twemoji)
under [CC BY 4.0](https://creativecommons.org/licenses/by/4.0/). See
[THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md) for the full attribution.

## Tech stack

| Module | Choice |
| ------ | ------ |
| UI | [Slint](https://slint.dev) (compiled pure Rust, no GC) |
| Async runtime | [`tokio`](https://tokio.rs) |
| SSH | [`russh`](https://crates.io/crates/russh) (vendored; no libssh) |
| SFTP | `russh-sftp` |
| Terminal emulation | `vt100` + custom render / find / highlight |
| Local PTY | `portable-pty` |
| Serial | `serialport` |
| Credentials | ChaCha20-Poly1305 + `keyring` (OS keyring) |
| Serialization | `serde` + `serde_json` |
| Logging | `tracing` + `tracing-subscriber` |
| i18n | Built-in gettext-style strings (`lang/`) |

## Running from source

Requires Rust **1.75+** (see `rust-version` in `Cargo.toml`), plus the Linux system packages above when applicable.

```bash
git clone https://github.com/zhuhezhang/zinterm.git
cd zinterm
cargo run --release
```

On first launch empty `sessions.json` / `commands.json` (and siblings) are created under the config directory.
Click **"＋ New Session"** in the top-right to add your first server, or open a saved session from the welcome page.

Useful commands:

```bash
# Dev build (faster compile, slower runtime)
cargo run

# Type-check only (fastest feedback after editing .slint)
cargo check

# Print version
cargo run --release -- --version
```

## Importing connections and password fields

**Export connections** writes a portable JSON file (`zinterm_export: "sessions"`),
but **does not** include `password`, `key_passphrase`, or `private_key`.
To migrate credentials, hand-edit those fields into the file before
**Import connections**.

Whether imported secrets are persisted follows **Settings → Data → Save passwords / keys**:

- **On**: keep the auth-specific secret fields and store them in the encrypted
  credentials vault (`zinterm-credentials-vault.json`, master key in the OS keyring).
- **Off**: ignore those fields even if present; connections still import, and
  you will be prompted on first connect.

### Fields (SSH only)

| Field | Meaning |
| ----- | ------- |
| `auth` | `"password"` (default) or `"key"` |
| `password` | **Password auth only**: login password |
| `key_passphrase` | **Key auth only**: passphrase for an encrypted private key (optional) |
| `private_key` | **Key auth only**: local key path **or** pasted key body (classified automatically) |

Put a multi-line private key in a **single JSON string**, separating lines with `\n`, for example:

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

> Plaintext passwords in an import file are only for one-shot migration; with “Save passwords”
> on they go into the local encrypted vault (ChaCha20-Poly1305, OS keyring master key). Do not commit JSON with plaintext secrets.

## Project layout

```
zinterm/
├── Cargo.toml
├── build.rs                 # Slint compiler entry point
├── lang/                    # zh / en strings (gettext .po / .mo)
├── scripts/                 # release helpers, etc.
├── ui/
│   ├── app.slint            # top-level window (only root file)
│   ├── shared/              # theme / widgets
│   ├── shell/               # title bar, tabs, dock area, etc.
│   ├── terminal/            # terminal view, SFTP, command bar, find
│   ├── welcome/             # welcome page / session list
│   ├── dialogs/             # dialogs
│   ├── interface/           # settings panel pages
│   ├── types/               # shared data structs
│   └── fonts/
└── src/
    ├── main.rs
    ├── app.rs / app/        # UI ↔ backend bridge, callbacks, splits
    ├── config/              # sessions / prefs / credentials vault
    ├── ssh/                 # SSH session, auth, known hosts, PPK
    ├── sftp/                # SFTP browse and transfer
    ├── terminal/            # PTY, Telnet, serial, render, ZMODEM
    ├── layout/              # split-pane layout tree
    ├── i18n/                # runtime language switch
    ├── wallpaper/           # custom wallpaper
    └── logging/             # tracing init and error log
```

## Development notes

- Slint widgets use a strict layout DSL; after editing a `.slint` file, `cargo check` is the fastest feedback loop.
- The application event loop is single-threaded (required by Slint); all
  cross-thread UI updates go through `slint::invoke_from_event_loop` callbacks.
- SSH / SFTP share the known-hosts path (`zinterm-known-hosts.json` fingerprints): first contact asks for
  trust and remembers the host key, while later key changes prompt again.
- `russh` is vendored under `vendor/russh` (compatibility patches, e.g. some device banners / MACs);
  check that tree and the notes in `Cargo.toml` before changing SSH behavior.
- Before publishing, use the release script below so the tag matches `Cargo.toml`.

## Release

Use the release helper so the tag points at a commit whose Cargo package version
matches the tag. Pass the git remote with `-Remote` / `--remote` (it may not be `origin`).

Windows (PowerShell):

```powershell
.\scripts\release.ps1 v0.6.0 -Push -Remote zinterm_github
```

macOS / Linux:

```bash
./scripts/release.sh v0.6.0 --push --remote zinterm_github
```

The script:

- requires no uncommitted tracked-file changes
- updates `Cargo.toml` and the `zinterm` entry in `Cargo.lock`
- runs `cargo check --locked`
- verifies that `zinterm --version` matches the tag
- commits the version bump
- creates an annotated tag
- pushes the current branch and tag to the given remote when `-Push` / `--push` is passed

To prepare the commit and tag without pushing:

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

The release workflow also checks pushed tags. A tag named `v0.6.0` must match
`Cargo.toml`, `Cargo.lock`, and the built `zinterm --version` output,
otherwise the workflow fails before publishing.

Mirror (optional): `https://gitee.com/zhuhezhang/zinterm`.

## License

Licensed under MIT. See [LICENSE](LICENSE).
