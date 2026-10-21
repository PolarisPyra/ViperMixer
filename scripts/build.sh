#!/usr/bin/env bash
# Builds Viper Mixer. `appimage` writes Viper-Mixer-x86_64.AppImage. `install` builds the release
# binary and installs it under $PREFIX (default ~/.local) with its icon and a launcher.
#
# Usage: scripts/build.sh appimage | install
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

die() {
  echo "$*" >&2
  exit 1
}

usage() {
  cat <<'EOF'
Usage: scripts/build.sh appimage | install

  appimage  Build target/appimage/Viper-Mixer-x86_64.AppImage
  install   Build the release binary and install it under $PREFIX (default ~/.local):
            bin/viper-midi-mixer, the icon, and share/applications/viper-midi-mixer.desktop
EOF
}

# The launcher's name matches the app id the window reports, so the desktop shows its icon.
app_id="viper-midi-mixer"
version="$(awk -F '"' '/^version = / { print $2; exit }' Cargo.toml)"

require_tools() {
  local tool
  for tool in "$@"; do
    command -v "$tool" > /dev/null || die "$tool is required."
  done
}

build_appimage() {
  require_tools cargo curl
  [[ "$(uname -m)" == x86_64 ]] || die "AppImage packaging supports x86_64; detected $(uname -m)."

  local tools_dir="${XDG_CACHE_HOME:-$HOME/.cache}/viper-midi-mixer/appimage-tools"
  local linuxdeploy="$tools_dir/linuxdeploy-x86_64.AppImage"
  local appimage_dir="$repo_root/target/appimage"
  local appdir="$appimage_dir/AppDir"
  local out="$appimage_dir/Viper-Mixer-x86_64.AppImage"

  mkdir -p "$tools_dir" "$appimage_dir"
  if [[ ! -x "$linuxdeploy" ]]; then
    curl --fail --location --retry 3 \
      https://github.com/linuxdeploy/linuxdeploy/releases/download/continuous/linuxdeploy-x86_64.AppImage \
      --output "$linuxdeploy"
    chmod +x "$linuxdeploy"
  fi

  cargo build --release --locked
  rm -rf "$appdir"
  # linuxdeploy matches the desktop file's Icon= name against the icon file name.
  local icon="$appimage_dir/$app_id.png"
  cp assets/logo.png "$icon"

  (
    cd "$appimage_dir"
    LDAI_OUTPUT="$out" LDAI_VERSION="$version" APPIMAGE_EXTRACT_AND_RUN=1 "$linuxdeploy" \
      --appdir "$appdir" \
      --executable "$repo_root/target/release/viper-midi-mixer" \
      --desktop-file "$repo_root/packaging/viper-midi-mixer.desktop" \
      --icon-file "$icon" \
      --output appimage
  )
  [[ -f "$out" ]] || die "Expected $out after the build."
  echo "AppImage: $out"
}

install_app() {
  require_tools cargo
  local prefix="${PREFIX:-$HOME/.local}"
  local bin_path="$prefix/bin/viper-midi-mixer"
  local apps_dir="$prefix/share/applications"
  local icons_dir="$prefix/share/icons/hicolor"
  local launcher="$apps_dir/$app_id.desktop"

  cargo build --release --locked

  install -Dm755 target/release/viper-midi-mixer "$bin_path"
  echo "Binary: $bin_path"

  install -Dm644 assets/logo.png "$icons_dir/512x512/apps/$app_id.png"
  echo "Icon: $icons_dir/512x512/apps/$app_id.png"

  # The launcher's Exec is the absolute binary path, so it goes stale when the prefix or the
  # desktop file changes. Compare the rendered launcher with the installed copy before rewriting it.
  local rendered
  rendered="$(sed -e "s|^Exec=.*|Exec=$bin_path|" -e "s|^TryExec=.*|TryExec=$bin_path|" \
    packaging/viper-midi-mixer.desktop)"
  mkdir -p "$apps_dir"
  if [[ ! -f "$launcher" ]]; then
    echo "Launcher: new install, writing $launcher"
  elif [[ "$rendered" == "$(cat "$launcher")" ]]; then
    echo "Launcher: up to date"
  else
    echo "Launcher: stale, rewriting $launcher"
  fi
  printf '%s\n' "$rendered" > "$launcher"
  if command -v desktop-file-validate > /dev/null; then
    desktop-file-validate "$launcher"
  fi

  refresh_caches "$apps_dir" "$icons_dir"

  case ":$PATH:" in
    *":$prefix/bin:"*) ;;
    *) echo "Note: $prefix/bin is not on PATH, so the 'viper-midi-mixer' command won't resolve." ;;
  esac
  echo "Installed Viper Mixer $version. Open it from the application menu; it runs in the tray."
}

refresh_caches() {
  local apps_dir="$1" icons_dir="$2"
  if command -v update-desktop-database > /dev/null; then
    update-desktop-database -q "$apps_dir"
  fi
  if command -v gtk-update-icon-cache > /dev/null && [[ -f "$icons_dir/index.theme" ]]; then
    gtk-update-icon-cache -q -t -f "$icons_dir"
  fi
  # Plasma 6 keeps its own application cache, which is what the KDE menu reads.
  if command -v kbuildsycoca6 > /dev/null; then
    kbuildsycoca6 --noincremental > /dev/null 2>&1 \
      || echo "Warning: kbuildsycoca6 failed; the KDE menu may update after you log in again."
  fi
}

case "${1:-}" in
  appimage) build_appimage ;;
  install) install_app ;;
  -h | --help) usage ;;
  *)
    usage >&2
    exit 1
    ;;
esac
