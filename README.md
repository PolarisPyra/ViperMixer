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
./packaging/build-appimage.sh
```

The output is written to `target/appimage/`. The first run downloads linuxdeploy
into `~/.cache/viper-midi-mixer/appimage-tools/`; later runs reuse it. This
builds an AppImage without installing system files or requiring root.

## Install on Arch Linux

Install the release binary, searchable desktop entry, and tray/application icons system-wide:

```sh
./packaging/packenger.sh install
```

Remove those files later with `./packaging/packenger.sh uninstall`. Your mixer
settings in `~/.config/pipewire-mixer/` are kept when uninstalling. The script
uses `sudo` when needed; you can also run it as root.

## MIDI controls

- MIDI CC 1–5 set fader levels 1–5.
- Mute notes are configurable from each fader card with **Bind mute key**.
- Choose a MIDI device from the input menu, or use **Automatic** to connect to
  the first available input. **Disconnected** stops MIDI control until you
  choose another option.
- App assignments are saved in `~/.config/pipewire-mixer/config.json`.

The window and tray icons use `assets/logo.png`.

When a desktop tray is available, the app starts as a tray icon without opening
a window. Use **Show Viper Mixer** to open its window. **Close to system tray**
controls whether closing the window keeps the app running in the tray. Use
**Quit** in the tray menu to exit the app.
