<a href="https://github.com/PolarisPyra/ViperMixer/releases/latest"><img align="right" src="https://img.shields.io/github/v/release/PolarisPyra/ViperMixer?label=version" alt="Latest version"></a>

# Viper Mixer

A native Rust and egui mixer for MIDI hardware faders on Linux. Assign five MIDI
faders to PipeWire audio applications, map mute notes, and keep assignments
across application restarts. The app checks for new audio streams and MIDI
devices while it runs, and it includes a Steam Games assignment for current and
future Steam game audio.
New app assignments are reconnected with PipeWire application metadata when
stream node IDs change; existing Python settings continue to match by app name.

## Build and run

Install Rust, ALSA development files, and the PipeWire command line tools
(`pw-dump` and `wpctl`), then run:

```sh
cargo run --release
```

Build a release binary with `cargo build --release`. The executable is written
to `target/release/viper-midi-mixer`.

List MIDI inputs from the command line with `cargo run --release -- --list-midi`.

## Build an AppImage

Build a distributable AppImage for x86_64 Linux:

```sh
scripts/build.sh appimage
```

The output is `target/appimage/Viper-Mixer-x86_64.AppImage`, under the same name for every
version. The first run downloads linuxdeploy into `~/.cache/viper-midi-mixer/appimage-tools/`;
later runs reuse it. This builds an AppImage without installing system files or requiring root.

## Install locally

Install the release binary, its icon, and a searchable launcher for your user:

```sh
scripts/build.sh install
```

The files go under `~/.local` (`bin/viper-midi-mixer`, the hicolor icon, and
`share/applications/viper-midi-mixer.desktop`). Set `PREFIX` to install somewhere else. The
script reports whether the launcher is new, up to date, or stale and rewritten, and refreshes the
desktop, icon, and KDE caches so the menu picks it up. Your mixer settings in
`~/.config/viper-mixer/` are not touched.

## MIDI controls

- MIDI CC 1–5 set fader levels 1–5.
- Mute notes are configurable from each fader card with **Bind mute key**.
- Choose a MIDI device from the input menu, or use **Automatic** to connect to
  the first available input. **Disconnected** stops MIDI control until you
  choose another option.
- App assignments are saved in `~/.config/viper-mixer/config.json`.

The window and tray icons use `assets/logo.png`.

When a desktop tray is available, closing the window keeps the app running in
the tray. Use **Show Viper Mixer**, or launch the app again from the application menu, to reopen or bring
forward its window.
**Close to system tray** controls whether closing the window keeps the app
running in the tray. Use **Quit** in the tray menu to exit the app.

## Releases

Changelog entries and release notes come from the commit history with
[git-cliff](https://git-cliff.org/), configured in `cliff.toml`. Each commit is sorted into
a group by the start of its subject line, and commits that start with `Release`, `Bump`,
`Merge`, or `Chore` are left out. Words that don't match a group go under **Other**, so write
commit subjects with a leading verb, for example `Add ...`, `Fix ...`, or `Change ...`.

The release script sets the version, so you never edit it in `Cargo.toml` by hand. The version
in the repo always matches the latest tag.

### Version numbers

Versions are `MAJOR.MINOR.PATCH` with no leading zeros and no suffixes. Read the current version
with `git describe --tags --abbrev=0`, then raise one part by one:

- **Patch** for fixes and small changes: `0.5.1` → `0.5.2`.
- **Minor** for larger features, resetting the patch: `0.5.1` → `0.6.0`.
- **Major** only for a deliberate major release.

Hotfixes are ordinary patch releases: a hotfix after `0.5.9` is `0.5.10`, not `0.5.91`. Each
part is compared as a whole number, so `0.5.91` would sort above every release from `0.5.10` to
`0.5.90` and break release ordering.

### Cutting a release

Run the script from a clean `main` that is up to date with `origin/main`:

```sh
scripts/release.sh 0.3.0
```

The script checks the version and the repo state, runs Clippy, and prints the release notes for
the new version. After you confirm, it writes the version into `Cargo.toml` with
`scripts/set-version.sh`, updates `Cargo.lock`, regenerates `CHANGELOG.md` with a full rewrite,
commits them as `Release v0.3.0`, creates the annotated tag, and pushes `main` and the tag.
Nothing is written until you confirm.

Pushing the tag runs `.github/workflows/release-appimage.yml`. It runs `scripts/set-version.sh`
with the tag's version, so a tag pushed without the script still builds with its own version.
It then builds the AppImage, attaches it to a GitHub release named after the tag, and uses
git-cliff for the release notes. To check the build without a release, run the workflow by hand
from the Actions tab (**Run workflow**).

Don't edit `CHANGELOG.md` by hand. Tags must start with `v` followed by a digit, such as
`v0.3.0`. Never reuse, move, or delete a pushed tag; a broken release is followed by the next
patch release.
