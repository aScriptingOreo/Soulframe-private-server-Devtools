#!/usr/bin/env bash
# Linux version of setup.cmd (Arch, Ubuntu/Debian): a private copy of Soulframe that runs in Wine as its own user
# "sfplay", and an nftables rule so that user can only reach the host in sf-server.txt (and loopback).
# Your real game is only read (copied), never changed or started.
# Usage: bash setup.sh [folder with Soulframe.x64.exe] [where the copy goes: .../SoulframePrivate]     (asks for sudo)
#        The copy is owned by sfplay with your group allowed in, so you can reach it (unlike under /home/sfplay).
# Undo:  sudo systemctl disable --now sf-copy-firewall; sudo userdel -r sfplay;
#        sudo rm /etc/sf-copy.nft /etc/systemd/system/sf-copy-firewall.service /usr/local/bin/sf-play \
#          /etc/sysctl.d/60-sf-server.conf /etc/sudoers.d/sf-play
#        and delete the copy's SoulframePrivate folder if you put it somewhere else
#        (and sudo setfacl -x u:sfplay on the folders above it)
set -euo pipefail
P16H6=3be7694b589a7e2ae92a53b3e26ed8c3bcfac8c34ff25d69045df374b35e4765  # Soulframe.x64.exe, build 2026.10.08.15.20
[[ $EUID -eq 0 ]] || exec sudo bash "$0" "$@"
me=${SUDO_USER:?run this from your normal account (it uses sudo itself)}
myhome=$(getent passwd "$me" | cut -d: -f6)
here=$(cd "$(dirname "$0")" && pwd)
user=sfplay
home=/home/$user
dest=$(realpath -m "${2:-$home/SoulframePrivate}")
# Everything in it is chowned to sfplay below, so it must be a folder of its own, never one of yours.
[[ $(basename "$dest") == SoulframePrivate ]] || { echo "The copy's folder must be named SoulframePrivate (got $dest)."; exit 1; }
as_user() { sudo -u "$user" env HOME="$home" WINEPREFIX="$home/.wine" WINEDEBUG=-all "$@"; }

# Wine resolves names inside the game, and the firewall blocks DNS for it, so the copy gets the IP, not a name.
# In the sf-join package everything is next to this script; in a clone of the repo (setup.sh at the top) the
# launcher and DLL come from the cargo build, and with no sf-server.txt the server is this PC.
bin=$here
[[ -f $bin/sf-launch.exe ]] || bin=$here/../../bootstrap/target/x86_64-pc-windows-gnu/release
[[ -f $bin/sf_bootstrap.dll ]] || { echo "sf_bootstrap.dll not found. From a clone of the repo, run setup.sh at the top."; exit 1; }
server=127.0.0.1
[[ ! -f $here/sf-server.txt ]] || server=$(head -n1 "$here/sf-server.txt" | tr -d '\r[:space:]')
ip=$(getent ahostsv4 "$server" | awk 'NR == 1 { print $1 }')
[[ $ip ]] || { echo "Could not find an IPv4 address for '$server' (sf-server.txt)."; exit 1; }

# The only distro-specific part.
. /etc/os-release
case " $ID ${ID_LIKE:-} " in
  *" arch "*)
    pacman -S --needed --noconfirm wine curl nftables xorg-xhost libpulse acl python ||
      { echo "pacman failed. Wine is in [multilib]: enable it in /etc/pacman.conf, then run setup.sh again."; exit 1; } ;;
  *" debian "* | *" ubuntu "*)
    dpkg --add-architecture i386
    apt-get update
    apt-get install -y wine64 wine32:i386 curl nftables x11-xserver-utils pulseaudio-utils acl python3 ;;
  *)
    echo "Unknown distro '$ID'. Install wine, curl, nftables, xhost and pactl yourself, then set SF_SKIP_PACKAGES=1."
    [[ ${SF_SKIP_PACKAGES:-} ]] || exit 1 ;;
esac

id "$user" &>/dev/null || useradd -m -s /usr/sbin/nologin "$user"
for g in video render audio; do getent group $g >/dev/null && usermod -aG $g "$user"; done

if [[ ! -f $dest/Soulframe.x64.exe ]]; then
  src=${1:-}
  for c in "$myhome"/.local/share/Steam/steamapps/common/Soulframe \
           "$myhome"/.var/app/com.valvesoftware.Steam/.local/share/Steam/steamapps/common/Soulframe \
           "$myhome"/.local/share/Steam/steamapps/compatdata/*/pfx/drive_c/users/*/AppData/Local/Soulframe/Downloaded/Public \
           "$myhome"/.wine/drive_c/users/*/AppData/Local/Soulframe/Downloaded/Public \
           "$myhome"/Games/*/drive_c/users/*/AppData/Local/Soulframe/Downloaded/Public; do
    [[ $src || ! -f $c/Soulframe.x64.exe ]] || src=$c
  done
  while [[ ! -f $src/Soulframe.x64.exe ]]; do
    read -rp "Soulframe install folder (the one with Soulframe.x64.exe): " src
  done
  # The game's exe and DE's launcher by path, not "Soulframe*": that also matches SoulframePrivateServer (the GUI).
  ! pgrep -if 'soulframe\.x64\.exe|soulframe.*launcher\.exe' >/dev/null || { echo "Close Soulframe and its launcher first."; exit 1; }
  [[ $(sha256sum "$src/Soulframe.x64.exe" | cut -d' ' -f1) == "$P16H6" ]] ||
    { echo "Your Soulframe is a different version than this server supports (P16H6, 2026.10.08). Nothing was changed."; exit 1; }
  echo "Copying $src to $dest (about 13 GB unless btrfs/XFS share it; read-only on your real install)..."
  mkdir -p "$dest"
  # Copy-on-write where the filesystem can (btrfs, XFS): no extra space and instant, and the game's writes to its
  # caches still split off from your real install. Elsewhere a plain copy. Not symlinks: the game writes Cache.Windows.
  cp -r --reflink=auto "$src/." "$dest"
fi
[[ $(sha256sum "$dest/Soulframe.x64.exe" | cut -d' ' -f1) == "$P16H6" ]] || { echo "$dest is not build P16H6."; exit 1; }
cp "$bin/sf-launch.exe" "$bin/sf_bootstrap.dll" "$dest"
echo "$ip" > "$dest/sf-server.txt"
chown -R "$user:" "$home"
chown -R "$user:$(id -gn "$me")" "$dest"
chmod -R g+rwX "$dest"
# sfplay has to get through every folder above the copy, and home folders are usually 700. An ACL lets just sfplay
# pass through (search, not list or read), instead of chmod o+x for everyone.
parents=() d=$dest
while d=$(dirname "$d"); [[ $d != / ]]; do parents=("$d" "${parents[@]}"); done  # top down
for d in "${parents[@]}"; do
  as_user test -x "$d" || setfacl -m "u:$user:x" "$d" 2>/dev/null || true
done
as_user test -w "$dest/Soulframe.x64.exe" ||
  { echo "$user can't reach $dest (no ACL support on this filesystem?). Pick another place, or chmod o+x the folders above it."; exit 1; }

# Wine prefix with DXVK (the game is DirectX 11). DXVK is pinned and checked here: winetricks reads the latest version
# from the GitHub API and builds a broken URL (.../download/v/dxvk-.tar.gz) when that answer doesn't parse.
dxvk=3.1.1 dxvk_sha=40565b4a724aadc4433fa4e010b4b23916d9b1f1baeee64e17186db94f54e608
if [[ $(cat "$home/.wine/.sf-ready" 2>/dev/null) != "dxvk $dxvk" ]]; then
  as_user WINEDLLOVERRIDES='mscoree,mshtml=' wineboot -i
  tmp=$(mktemp -d)
  curl -fL --retry 3 -o "$tmp/dxvk.tar.gz" "https://github.com/doitsujin/dxvk/releases/download/v$dxvk/dxvk-$dxvk.tar.gz"
  echo "$dxvk_sha  $tmp/dxvk.tar.gz" | sha256sum -c --quiet || { echo "DXVK download is corrupt; run setup.sh again."; exit 1; }
  tar -xzf "$tmp/dxvk.tar.gz" -C "$tmp"
  for dll in d3d8 d3d9 d3d10core d3d11 dxgi; do
    install -o "$user" -m644 "$tmp/dxvk-$dxvk/x64/$dll.dll" "$home/.wine/drive_c/windows/system32/$dll.dll"
    as_user wine reg add 'HKEY_CURRENT_USER\Software\Wine\DllOverrides' /v "$dll" /d native /f >/dev/null
  done
  as_user wineserver -w
  rm -rf "$tmp"
  echo "dxvk $dxvk" | as_user tee "$home/.wine/.sf-ready" >/dev/null
fi

# Proton from your Steam, if there is one: sf-play prefers it to the Wine above (SF_RUNNER=wine sf-play skips it).
# Steam itself can't run the copy: it runs games as you, and only sfplay is firewalled. Copied, not used in place:
# sfplay can't read your home, and Proton unpacks itself into its own folder on first run.
pick() { printf '%s\n' "$@" | sort -V | tail -n1; }  # newest; GE-Proton10-1 after GE-Proton9-27
shopt -s nullglob
proton=
for steam in "$myhome/.local/share/Steam" "$myhome/.var/app/com.valvesoftware.Steam/.local/share/Steam"; do
  [[ $proton ]] || proton=$(pick "$steam"/compatibilitytools.d/GE-Proton*/proton)
  [[ $proton ]] || proton=$(pick "$steam"/steamapps/common/Proton\ [0-9]*/proton)
  [[ $proton ]] || proton=$(pick "$steam"/steamapps/common/Proton\ -\ Experimental/proton)
done
shopt -u nullglob
if [[ $proton ]]; then
  pdir=$(dirname "$proton")
  if [[ $(cat "$home/proton/version" 2>/dev/null) != "$(cat "$pdir/version" 2>/dev/null)" || ! -f $home/proton/proton ]]; then
    echo "Copying $(basename "$pdir") to $home/proton for sf-play (about 1 GB)..."
    rm -rf "$home/proton"
    cp -a "$pdir" "$home/proton"
  fi
  install -d -o "$user" "$home/proton-data"  # STEAM_COMPAT_DATA_PATH: Proton's own prefix
  chown -R "$user:" "$home/proton"
else
  echo "No Proton found in your Steam; sf-play uses Wine."
fi

# Everything user sfplay sends goes to loopback or the host, or is rejected. Loaded at every boot.
cat > /etc/sf-copy.nft <<EOF
table inet sf_copy
delete table inet sf_copy
table inet sf_copy {
  chain out {
    type filter hook output priority 0; policy accept;
    meta skuid "$user" oifname "lo" accept
    meta skuid "$user" ip daddr $ip accept
    meta skuid "$user" reject
  }
}
EOF
cat > /etc/systemd/system/sf-copy-firewall.service <<EOF
[Unit]
Description=Soulframe private copy: user $user may only reach $ip
Before=network-pre.target
Wants=network-pre.target

[Service]
Type=oneshot
RemainAfterExit=yes
ExecStart=$(command -v nft) -f /etc/sf-copy.nft
ExecStop=$(command -v nft) delete table inet sf_copy

[Install]
WantedBy=multi-user.target
EOF
systemctl daemon-reload
systemctl enable sf-copy-firewall
systemctl restart sf-copy-firewall
if sudo -u "$user" timeout 5 bash -c 'exec 3<>/dev/tcp/1.1.1.1/443' 2>/dev/null; then
  echo "The firewall rule did not take: $user can still reach the internet. Don't play; check 'sudo nft list ruleset'."
  exit 1
fi

sed "s|/home/sfplay/SoulframePrivate|$dest|" "$here/play.sh" > /usr/local/bin/sf-play
chmod 755 /usr/local/bin/sf-play
# For the GUI (server_gui.pyw / the AppImage), which has no terminal to ask for sudo in: the server binds ports 80
# and 443 as you, and sf-play's "sudo -u sfplay" needs no password (sfplay can do less than you can).
echo 'net.ipv4.ip_unprivileged_port_start=80' > /etc/sysctl.d/60-sf-server.conf
sysctl -q -p /etc/sysctl.d/60-sf-server.conf
echo "$me ALL=($user) NOPASSWD: ALL" > /etc/sudoers.d/sf-play
chmod 440 /etc/sudoers.d/sf-play
visudo -cqf /etc/sudoers.d/sf-play || { rm -f /etc/sudoers.d/sf-play; echo "Could not add the sudoers rule for sf-play."; }
echo
echo "Done. Play: run  sf-play  in a terminal as yourself (never Tools/Launcher.exe, never with your real Soulframe open)."
echo "Log in with a made-up email and password, NOT your Digital Extremes login."
