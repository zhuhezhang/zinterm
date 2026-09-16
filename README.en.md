# ZinTerm

[简体中文](./README.md) | **English**

A lightweight, low-memory SSH / terminal client inspired by FinalShell, but
written entirely in **Rust + [Slint](https://slint.dev)**. The goal is to keep
FinalShell's core experience (session management, tabbed terminals) while
cutting memory use from the 400 MB+ of a JVM app down to the tens-of-MB range of
a native binary.

## Screenshots

<p align="center">
  <img src="docs/screenshots/terminal-en.png" width="800"><br>
</p>

<p align="center">
  <img src="docs/screenshots/connection-en.png" width="800"><br>
</p>

<p align="center">
  <img src="docs/screenshots/settings-en.png" width="800"><br>
</p>

## Download & install

Every `v*` tag triggers a GitHub Actions build that produces native binaries for
**Windows / Linux / macOS**, published on the
[Releases](https://github.com/zhuhezhang/zinterm/releases) page.

### Windows

Download `zinterm-*-windows-x86_64.zip`, unzip, and run `zinterm.exe`.

### Linux

```bash
tar -xzf zinterm-*-linux-x86_64.tar.gz
cd zinterm-*-linux-x86_64
./zinterm                                  # run it directly
# Optional: install the app icon + launcher entry (shows the icon in the dock /
# app list — no argument needed, it finds the binary next to the script)
chmod +x install-linux.sh && ./install-linux.sh
```

> Requires glibc ≥ 2.35 (Ubuntu 22.04+ / Debian 12+). On Wayland you may need to
> log out/in once after installing the icon.

Building from source with `cargo run` on Linux Mint / Ubuntu / Debian requires
the Slint/winit/rfd system development packages:

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

The download is a `.zip` containing the `zinterm.app` bundle:

```bash
# Unzip (aarch64 = Apple Silicon, x86_64 = Intel)
unzip zinterm-*-macos-*.zip
# Move it to Applications (optional — it also runs in place)
mv zinterm.app /Applications/
# Clear the quarantine flag, otherwise macOS says "zinterm is damaged and can't be opened"
xattr -dr com.apple.quarantine /Applications/zinterm.app
# Open it (or double-click in Finder)
open /Applications/zinterm.app
```

> If you didn't move it to `/Applications`, point both paths above at wherever the `.app` actually is (e.g. `~/Downloads/zinterm.app`).

> To build from source, see [Running](#running) below.

## Features

### Done

- [x] FinalShell-style UI with dark / light / follow-system themes
- [x] Full VT/ANSI terminal emulation (btop / htop / vim render correctly)
- [x] Color emoji, including skin tones, flags, and ZWJ sequences
- [x] Tabs (welcome page + multiple sessions)
- [x] Session management: create / edit / delete / groups, local JSON, export / import
  - Config directory: `%APPDATA%/zinterm` (Windows)
    / `~/.config/zinterm` (Linux)
    / `~/Library/Application Support/zinterm` (macOS)
  - `sessions.json`: sessions and empty groups (`empty_groups`); `commands.json`: quick commands, empty quick groups (`quick_empty_groups`), and history
  - Export files omit passwords / private keys; to import credentials see
    [Importing connections and password fields](#importing-connections-and-password-fields)
- [x] SSH (`russh`, pure Rust): password / private key / encrypted key (passphrase)
- [x] SFTP browser + upload / download (drag-and-drop) + in-terminal ZMODEM (`sz`) receive
- [x] Quick commands + command box (broadcast to all sessions) + command history
- [x] Serial / Telnet sessions
- [x] Session passwords encrypted at rest (ChaCha20-Poly1305 + OS keyring master key, `zinterm-credentials-vault.json`)
- [x] Known-hosts (`zinterm-known-hosts.json` fingerprint store) verification + first-connect confirmation
- [x] Split panes for tabbed terminals

Color emoji graphics are provided by [Twemoji](https://github.com/jdecked/twemoji)
under [CC BY 4.0](https://creativecommons.org/licenses/by/4.0/). See
[THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md) for the full attribution.


## Tech stack

| Module        | Choice                                                            |
| ------------- | ----------------------------------------------------------------- |
| UI            | [Slint](https://slint.dev) (compiled pure Rust, no GC)            |
| Async runtime | [`tokio`](https://tokio.rs)                                       |
| SSH protocol  | [`russh`](https://crates.io/crates/russh) (no libssh dependency)  |
| Serialization | `serde` + `serde_json`                                            |
| Logging       | `tracing` + `tracing-subscriber`                                  |

## Running

```bash
cargo run --release
```

On first launch empty `sessions.json` / `commands.json` (and siblings) are created under
`%APPDATA%/zinterm`. Click **"＋ New Session"** in the top-right
to add your first server.

## Importing connections and password fields

**Export connections** writes a portable JSON file (`zinterm_export: "sessions"`)
but **does not** include `password`, `key_passphrase`, or `private_key`.
To migrate credentials, hand-edit those fields into the file before
**Import connections**.

Whether imported secrets are persisted follows **Settings → Data → Save
passwords / keys**:

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

Put a multi-line private key in a **single JSON string**, separating lines with
`\n`, for example:

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

> Plaintext passwords in an import file are only for one-shot migration; with
> “Save passwords” on they go into the local encrypted vault (ChaCha20-Poly1305,
> master key in the OS keyring). Do not commit JSON that still contains plaintext secrets.

## Project layout

```
zinterm/
├── Cargo.toml
├── build.rs                 # Slint compiler entry point
├── ui/
│   ├── app.slint            # top-level window
│   ├── theme.slint          # design tokens
│   ├── widgets.slint        # reusable buttons / inputs
│   ├── tabs.slint           # top tab bar
│   ├── welcome.slint        # welcome page / quick connect
│   ├── session_dialog.slint # new / edit session dialog
│   └── terminal_view.slint  # terminal view (v0.1 line-buffered)
└── src/
    ├── main.rs
    ├── app.rs               # UI ↔ backend bridge
    ├── config.rs            # session JSON persistence
    └── ssh.rs               # SSH session worker
```

## Development notes

- Slint widgets use a strict layout DSL; after editing a `.slint` file,
  `cargo check` is the fastest feedback loop.
- The application event loop is single-threaded (required by Slint); all
  cross-thread UI updates go through `slint::invoke_from_event_loop` callbacks.
- SSH / SFTP share the known-hosts path (`zinterm-known-hosts.json` fingerprints): first contact asks for
  trust and remembers the host key, while later key changes prompt again.

## Release

Use the release helper so the tag points at a commit whose Cargo package version
matches the tag. Pass the git remote with `-Remote` / `--remote` (it may not be
`origin`).

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

## License

Licensed under MIT. See [LICENSE](LICENSE).
