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

When a desktop tray is available, the app starts as a tray icon without opening
a window. Use **Show Viper Mixer** to open its window. **Close to system tray**
controls whether closing the window keeps the app running in the tray. Use
**Quit** in the tray menu to exit the app.

## Releases

Changelog entries and release notes come from the commit history with
[git-cliff](https://git-cliff.org/), configured in `cliff.toml`. Each commit is sorted into
a group by the start of its subject line, and commits that start with `Release`, `Bump`,
`Merge`, or `Chore` are left out. Words that don't match a group go under **Other**, so write
commit subjects with a leading verb, for example `Add ...`, `Fix ...`, or `Change ...`.

The tag sets the app version, so you never bump `Cargo.toml` by hand. The version in the repo
stays at `0.2.0` as a placeholder, which means a local build reports that version.

To cut a release, run the script from a clean `main` that is up to date with `origin/main`:

```sh
scripts/release.sh 0.3.0
```

The script checks the version and the repo state, runs Clippy, and prints the release notes for
the new version. After you confirm, it regenerates `CHANGELOG.md` with a full rewrite, commits it
as `Release v0.3.0`, creates the annotated tag, and pushes `main` and the tag. Nothing is written
until you confirm.

Pushing the tag runs `.github/workflows/release-appimage.yml`. It builds the AppImage,
attaches it to a GitHub release named after the tag, and uses git-cliff for the release notes.
