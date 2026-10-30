#!/usr/bin/env bash
# Build dist/SoulframePrivateServer-x86_64.AppImage: the GUI (tools/server_gui.pyw) with the server, fixtures/,
# gamedata/ and the player setup (release/join + sf-launch.exe + sf_bootstrap.dll for Wine) inside. No game files.
# Needs what setup.sh installs (python 3.12+, rustup), plus tkinter (Arch: tk, Debian/Ubuntu: python3-tk) and curl.
set -euo pipefail
root=$(cd "$(dirname "$0")" && pwd)
b=$root/build
python3 -c 'import tkinter' 2>/dev/null || { echo "No tkinter: install tk (Arch) or python3-tk (Debian/Ubuntu)."; exit 1; }

[[ -x $b/venv/bin/python ]] || python3 -m venv "$b/venv"
"$b/venv/bin/pip" install -q --disable-pip-version-check pyinstaller cryptography cargo-zigbuild
export PATH=$b/venv/bin:~/.cargo/bin:$PATH
rustup toolchain install stable --profile minimal --target x86_64-pc-windows-gnu
cargo +stable zigbuild --release --target x86_64-pc-windows-gnu --manifest-path "$root/bootstrap/Cargo.toml"

# luau-compile 0.712 for the dev panel (devlua.py), Linux build of the same release tools/setup.ps1 pins for Windows.
luau=$root/tools/luau
if [[ ! -x $luau/luau-compile ]]; then
  mkdir -p "$b" "$luau"
  curl -fL --retry 3 -o "$b/luau-ubuntu.zip" https://github.com/luau-lang/luau/releases/download/0.712/luau-ubuntu.zip
  echo "e792aaac235555b4e826e7f849c8ac6dd977bb3ff2f6013912bf29c06b6e4ef4  $b/luau-ubuntu.zip" | sha256sum -c --quiet
  python3 -c 'import sys, zipfile; zipfile.ZipFile(sys.argv[1]).extract("luau-compile", sys.argv[2])' "$b/luau-ubuntu.zip" "$luau"
  chmod +x "$luau/luau-compile"
fi

# What release/join/setup.sh expects next to it (as in the sf-join package; no sf-server.txt = this PC).
rm -rf "$b/join" "$b/AppDir"
mkdir -p "$b/join"
cp "$root"/release/join/* "$root"/bootstrap/target/x86_64-pc-windows-gnu/release/{sf-launch.exe,sf_bootstrap.dll} "$b/join"

pyinstaller --noconfirm --onedir --name sf-server --collect-submodules cryptography \
  --distpath "$b/AppDir/usr/bin" --workpath "$b/pyi" --specpath "$b" \
  --add-data "$root/tools/*.py:tools" --add-data "$luau:tools/luau" --add-data "$root/dev:dev" \
  --add-data "$root/bootstrap/src/imgui.rs:bootstrap/src" --add-data "$root/fixtures:fixtures" --add-data "$root/gamedata:gamedata" --add-data "$b/join:join" \
  "$root/tools/server_gui.pyw"

cd "$b/AppDir"
printf '#!/bin/sh\nexec "$(dirname "$(readlink -f "$0")")/usr/bin/sf-server/sf-server" "$@"\n' > AppRun
chmod +x AppRun
cat > sf-server.desktop <<EOF
[Desktop Entry]
Type=Application
Name=Soulframe private server
Exec=sf-server
Icon=sf-server
Categories=Game;
EOF
echo '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 64 64"><rect width="64" height="64" rx="12" fill="#2b3a2f"/><text x="32" y="43" font-size="30" text-anchor="middle" fill="#e8dcc0" font-family="sans-serif">SF</text></svg>' > sf-server.svg

# ponytail: appimagetool is pinned by version, not hash; add a sha256 check if this ever leaves the dev machine.
tool=$b/appimagetool-1.9.0
[[ -x $tool ]] || { curl -fL --retry 3 -o "$tool" https://github.com/AppImage/appimagetool/releases/download/1.9.0/appimagetool-x86_64.AppImage; chmod +x "$tool"; }
mkdir -p "$root/dist"
APPIMAGE_EXTRACT_AND_RUN=1 ARCH=x86_64 "$tool" "$b/AppDir" "$root/dist/SoulframePrivateServer-x86_64.AppImage"
echo "Built $root/dist/SoulframePrivateServer-x86_64.AppImage"
