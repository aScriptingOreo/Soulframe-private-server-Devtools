# Soulframe private server devtools

A local server and launcher for playing a pinned Soulframe build (P16H6, `2026.10.08.15.20`) against your own
server instead of Digital Extremes' backend. Not affiliated with Digital Extremes.

No game files are in this repo: the game itself comes from your own install. `gamedata/` holds the package data the
server needs (extracted from the game, via soulframe-pipeline).

## Easiest: the app

`SoulframePrivateServer.exe` (Windows) or `SoulframePrivateServer-x86_64.AppImage` (Linux) is one window: pick your
Soulframe folder (any copy of build P16H6 with `Soulframe.x64.exe`; nothing is downloaded from Digital Extremes),
click **Set up private copy** once, then **Play** (it starts the server too). Windows: tick **Dev tools** for the
spawn / god mode / time-of-day window (the same panel is on F9 in game). Saves live in
`%LOCALAPPDATA%\SoulframePrivateServer\data` / `~/.local/share/soulframe-private-server/data`.
On Linux, setup installs Wine itself (Arch: enable `[multilib]` first); the AppImage may need `fuse2`. If your Steam
has Proton (GE-Proton preferred), setup copies the newest one for the game's sandbox user and Play uses it
(`SF_RUNNER=wine sf-play` for plain Wine). Adding the copy to Steam won't work: Steam runs it as you, outside the
firewall rule, and `sf-launch.exe` refuses to start then.

Build them: `powershell -ExecutionPolicy Bypass -File build.ps1` on Windows, `bash build.sh` on Linux (after
`setup.cmd` / `setup.sh` once, for the toolchains). Output goes to `dist/`. From a clone without building:
`pythonw tools/server_gui.pyw`.

## Setup, step by step

Everyone runs the server on their own PC. You need Soulframe on build P16H6 (2026.10.08), about 13 GB free for a
separate copy of the game, and git to clone this repo.

**Windows: once**

1. Clone the repo: `git clone https://github.com/VandendriesscheLander1/Soulframe-private-server-Devtools`
2. Close Soulframe and its launcher.
3. Double-click `setup.cmd` in the cloned folder. It:
   - installs what's missing with winget: Python 3.12, Rust, and the Visual Studio C++ build tools
     (a few GB, the first time takes a while; accept the prompts),
   - adds Microsoft Defender exclusions for the build folder and the launcher + DLL in the game copy (Defender flags
     the launcher because it injects the DLL into the game, as malware loaders do; one admin prompt),
   - builds `sf-launch.exe` and `sf_bootstrap.dll` (an existing Rust install is reused),
   - asks for admin, copies your game to `%USERPROFILE%\SoulframePrivate` (your real install is only read) and adds
     firewall rules so that copy can only reach your own PC. Steam/Epic install? It asks for the folder.

   If it stops after installing Python or Rust, close the window and double-click `setup.cmd` again.
   Another antivirus (Avast, Norton, ...) may also block `sf-launch.exe`: add the same exclusions there by hand.

**Windows: every time you play**

1. Double-click `play.cmd`. It opens the server in its own window and starts the game.
2. Log in with a made-up email and password. The first login creates your account; use the same one next time.
3. When you're done, close the game, then the server window.

**Linux (Arch, Ubuntu/Debian): once**

1. Clone the repo (as above). Arch: enable `[multilib]` in `/etc/pacman.conf` first (for Wine).
2. Close Soulframe and its launcher.
3. In the cloned folder run `bash setup.sh` as yourself (it asks for your sudo password). It:
   - installs Python, Rust (reuses yours if you have rustup) and zig, to build the Windows `sf-launch.exe` and
     `sf_bootstrap.dll` for Wine,
   - installs DXVK 3.1.1 into the Wine prefix (needs a Vulkan-capable GPU driver),
   - installs Wine and nftables, creates a user `sfplay`, copies your game (Steam, Lutris or a Wine prefix; it asks
     if it can't find it) to `/home/sfplay/SoulframePrivate`, and adds a firewall rule so `sfplay` can only reach
     this PC.

**Linux: every time you play**

1. In the cloned folder run `bash play.sh`. It starts the game and runs the server in that terminal.
2. Log in with a made-up email and password, as on Windows.
3. When you're done, close the game, then press Ctrl+C in the terminal.

**Updating:** `git pull`, then run `setup.cmd` / `setup.sh` again (it rebuilds and skips the game copy).

**Saves** are in `data/accounts` in the cloned folder. Back that folder up to keep progress.

**Uninstall:** see [release/join/README.txt](release/join/README.txt).

Never log in with your real Digital Extremes account on a private server, and don't run the private copy
alongside the real game.

## Later: one shared host

Run `tools\make-packages.ps1 -Server <host's Tailscale 100.x IP>`. The host runs `host.cmd --host` from `dist\sf-host`
and players get only `dist\sf-join`. See [release/host/README.txt](release/host/README.txt).

## Layout

| Path | What |
|---|---|
| `setup.cmd`, `play.cmd`, `setup.sh`, `play.sh` | Set up once, then play (above) |
| `tools/dummy_server.py` | The server (login, inventory, fixtures, hub relay); makes `certs/` on first run |
| `tools/admin.py` | Account admin (`python tools/admin.py accounts`) |
| `tools/shop_rolls.py`, `tools/vendor.py`, `gamedata/` | Shop stock and weapon temper rolls, from the game's package data |
| `tools/devgui.pyw`, `tools/devlua.py`, `dev/dev.lua` | In-game dev tools (Windows; F9 in game, or `pythonw tools\devgui.pyw` while playing). `setup.cmd` downloads the Luau compiler they use to `tools\luau` |
| `tools/stock_to_game.py`, `tools/luau_to_stock.py` | Turn Luau bytecode into the game's form (from soulframe-rust-workspace), with `gamedata/opcode-map.json` and `corpus_atoms.json` |
| `tools/*.ps1` | Pin a build, make an isolated copy, redirect, launch, build packages |
| `bootstrap/` | Rust: `sf-launch.exe` and `sf_bootstrap.dll` |
| `release/host`, `release/join` | Packages for a shared host and its players (`tools/make-packages.ps1`) |
| `fixtures/` | Response shapes the server replays |
| `builds/P16H6/manifest.sha256` | Hashes used to check the pinned build |
