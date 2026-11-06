#!/usr/bin/env bash
# Renders a demo scene to a PNG: opens `viper-mixer --demo SCENE` in a headless KWin with its own
# D-Bus session and has Spectacle save the window. Everything in that session, including services
# D-Bus starts there, sees only the headless display, so the desktop this runs from is never
# captured, and whatever is still running on that bus at the end is stopped. Needs kwin_wayland and
# spectacle (KDE Plasma).
#
# Usage: scripts/screenshot.sh [OUT.png] [SCENE]   (default docs/screenshot.png, scene default)
set -euo pipefail

out=$(realpath -m "${1:-docs/screenshot.png}")
scene=${2:-default}
cd "$(dirname "$0")/.."

for tool in kwin_wayland spectacle dbus-run-session; do
  command -v "$tool" >/dev/null || { echo "$tool is needed to take the screenshot." >&2; exit 1; }
done
cargo build --release --quiet
rm -f "$out"

# The socket name is per process, so two runs don't share a compositor.
export SNAP_SOCKET="viper-mixer-snap-$$" SNAP_OUT="$out" SNAP_SCENE="$scene"
timeout 60 env -u DISPLAY -u WAYLAND_DISPLAY dbus-run-session -- bash -c '
  kwin_wayland --virtual --no-lockscreen --socket "$SNAP_SOCKET" --width 1000 --height 800 >/dev/null 2>&1 &
  kwin=$!
  export WAYLAND_DISPLAY="$SNAP_SOCKET"
  sleep 2
  ./target/release/viper-mixer --demo "$SNAP_SCENE" >/dev/null 2>&1 &
  app=$!
  sleep 3
  spectacle --background --nonotify --activewindow --no-decoration --output "$SNAP_OUT" >/dev/null 2>&1 || true
  kill "$app" "$kwin" 2>/dev/null
  wait "$app" "$kwin" 2>/dev/null || true
  for environ in /proc/[0-9]*/environ; do
    if grep -qzx "DBUS_SESSION_BUS_ADDRESS=$DBUS_SESSION_BUS_ADDRESS" "$environ" 2>/dev/null; then
      pid=${environ#/proc/}; pid=${pid%/environ}
      [[ $pid != "$$" ]] && kill "$pid" 2>/dev/null
    fi
  done
  true' </dev/null 2>/dev/null

[[ -s "$out" ]] || { echo "No screenshot was saved." >&2; exit 1; }
echo "Saved $out"
