#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)

if ! command -v sudo >/dev/null 2>&1 && (( EUID != 0 )); then
    printf 'Run as root or install sudo to continue.\n' >&2
    exit 1
fi

if ! command -v cargo >/dev/null 2>&1; then
    printf 'Cargo was not found. Install the Rust toolchain before running this script.\n' >&2
    exit 1
fi

if (( EUID == 0 )); then
    as_root=()
else
    as_root=(sudo)
fi

cargo build --release --locked --manifest-path "$repo_root/Cargo.toml"

"${as_root[@]}" install -Dm755 "$repo_root/target/release/viper-midi-mixer" /usr/bin/viper-midi-mixer
"${as_root[@]}" install -Dm644 "$repo_root/packaging/viper-midi-mixer.desktop" /usr/share/applications/viper-midi-mixer.desktop
"${as_root[@]}" install -Dm644 "$repo_root/assets/logo.png" /usr/share/icons/hicolor/512x512/apps/viper-midi-mixer.png

if command -v update-desktop-database >/dev/null 2>&1; then
    "${as_root[@]}" update-desktop-database /usr/share/applications
fi

printf 'Viper Mixer installed. Launch it with `viper-midi-mixer` or from your application menu.\n'
