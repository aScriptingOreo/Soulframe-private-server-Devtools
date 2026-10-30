#!/usr/bin/env bash
# Start the private copy set up by setup.sh (installed as sf-play). Run as yourself: the game runs as user sfplay,
# on your screen and speakers. sf-launch.exe refuses to start if sfplay can reach the internet.
set -euo pipefail
[[ $EUID -ne 0 ]] || { echo "Run sf-play as your normal user, not root."; exit 1; }
systemctl is-active -q sf-copy-firewall || { echo "Firewall rule not loaded: sudo systemctl start sf-copy-firewall"; exit 1; }
xhost +SI:localuser:sfplay >/dev/null  # let sfplay draw on your screen (X11, or XWayland)
# sfplay can't open your PulseAudio/PipeWire socket, so share it on loopback (which the firewall allows).
pactl list modules short | grep -q module-native-protocol-tcp ||
  pactl load-module module-native-protocol-tcp port=4713 auth-ip-acl=127.0.0.1 auth-anonymous=1 >/dev/null ||
  echo "No sound: pactl could not share your audio."
cd /  # sfplay may not be able to read your current folder; Wine warns "could not find DOS drive" then
exe=/home/sfplay/SoulframePrivate/sf-launch.exe  # setup.sh puts the copy's real place here
run=(sudo -u sfplay env HOME=/home/sfplay DISPLAY="${DISPLAY:?no display}" PULSE_SERVER=tcp:127.0.0.1:4713)
# Proton (copied from your Steam by setup.sh) when there is one, outside Steam; SF_RUNNER=wine for the system Wine.
if [[ ${SF_RUNNER:-} != wine ]] && sudo -u sfplay test -f /home/sfplay/proton/proton; then
  echo "Starting with Proton ($(sudo -u sfplay cat /home/sfplay/proton/version 2>/dev/null))"
  exec "${run[@]}" STEAM_COMPAT_DATA_PATH=/home/sfplay/proton-data STEAM_COMPAT_CLIENT_INSTALL_PATH=/home/sfplay \
    python3 /home/sfplay/proton/proton run "$exe"
fi
exec "${run[@]}" WINEPREFIX=/home/sfplay/.wine WINEDEBUG=-all wine "$exe"
