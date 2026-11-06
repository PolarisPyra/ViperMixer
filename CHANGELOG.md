# Changelog

All notable changes to Viper Mixer are documented here, for the people who use it.
Each release adds its notes at the top; see docs/release.md.

## [26.11.1] - 2026-11-06

### Changed

- Viper Mixer has a warm amber accent and a dark icon, so it no longer looks like Viper Music on the taskbar or side by side.
- Viper Mixer looks like a hardware console. Each strip has a power button that mutes it when pressed and glows red while it's muted, its level on a black bar, and a ribbed fader beside a column of lights. Check for updates is now in the header.

### Fixed

- In a large or maximized window, everything grows evenly to fill it, so the faders no longer stretch out of shape.

### Removed

- The Latch and Toggle choice is gone from the footer. A mute on the controller always follows its switch: down mutes the strip and up unmutes it.

## [26.11.0] - 2026-11-06

### Added

- Viper Mixer checks for updates shortly after it starts and then every hour, and Check for updates in the footer checks right away. Installed as an AppImage, it downloads and checks the new version itself, and a banner under the header offers to restart into it.

### Changed

- Viper Mixer has been rebuilt with a new look and a new logo. Each strip now learns its fader and its mute button from your controller, can drive your output device or microphone, and finds your controller again after you replug it.

## [0.5.3] - 2026-10-24

### Fixed

- Fix the release workflow's version step so the workflow file parses

## [0.5.2] - 2026-10-24

### Added

- Add CLAUDE.md with the release procedure

### Changed

- Change the CLAUDE.md PATH note for the Bash tool
- Change releases to set the version from the release script

### Fixed

- Fix reopening Viper Mixer from the app menu while it runs in the tray

## [0.5.1] - 2026-10-21

### Changed

- Change the tray to open its menu on left click

## [0.5.0] - 2026-10-21

### Added

- Add build and release scripts

### Other

- Match ALSA streams by program and reopen the window from the tray

## [0.1.1] - 2026-10-21

### Other

- Skip AppImage build for tags that already have a release

## [0.1.0] - 2026-10-21

### Added

- Add Viper MIDI Mixer app and AppImage builder
- Show mixer in application launcher
- Add AppImage release workflow with git-cliff notes

### Changed

- Use viper-mixer config directory
- Update app icon to native 512px asset

### Fixed

- Fix close to tray and rename app to Viper Mixer
- Fix mixer tray close and reopen lifecycle
- Fix faders driving every node of an app
- Fix release notes output in AppImage workflow

### Other

- First commit
- Keep mixer in tray and enforce single instance
- Combine Arch install and uninstall script
- Shut down mixer engine on app exit
- Logo change
- Speed up mixer startup
- Keep mixer running when window closes
- Logo
