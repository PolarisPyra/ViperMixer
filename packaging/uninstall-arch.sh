#!/usr/bin/env bash
set -euo pipefail

if ! command -v sudo >/dev/null 2>&1 && (( EUID != 0 )); then
    printf 'Run as root or install sudo to continue.\n' >&2
    exit 1
fi

if (( EUID == 0 )); then
    as_root=()
else
    as_root=(sudo)
fi

"${as_root[@]}" rm -f \
    /usr/bin/viper-midi-mixer \
    /usr/share/applications/viper-midi-mixer.desktop \
    /usr/share/icons/hicolor/256x256/apps/viper-midi-mixer.png \
    /usr/share/icons/hicolor/512x512/apps/viper-midi-mixer.png

if command -v update-desktop-database >/dev/null 2>&1; then
    "${as_root[@]}" update-desktop-database /usr/share/applications
fi

if (( EUID != 0 )) && command -v kbuildsycoca6 >/dev/null 2>&1; then
    kbuildsycoca6 --noincremental
fi

printf 'Viper Mixer was removed. Settings were left untouched.\n'
