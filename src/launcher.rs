//! The desktop's launcher entry and icon for the running AppImage, brought up to date at each start.
//!
//! The window reports the app id `viper-mixer`, and KWin draws the title bar icon from the launcher
//! named after it, `viper-mixer.desktop`. An AppImage manager may add the app under another name
//! (Gear Lever writes `viper_mixer.desktop`) with an icon copied out of the AppImage at the time,
//! which an update never replaces. So each start of an AppImage writes the current icon into the
//! icon theme and over the icon any Viper Mixer launcher points at, adds a hidden
//! `viper-mixer.desktop` when there is no launcher by that name, and has the desktop read them again.

use std::path::{Path, PathBuf};

const APP_ID: &str = "viper-mixer";
/// `assets/logo.png`, rendered from `assets/logo.svg` (see the README).
const LOGO: &[u8] = include_bytes!("../assets/logo.png");
/// Marks the hidden launcher this module writes, so it is kept up to date and nothing else is replaced.
const OWN_KEY: &str = "X-Viper-Mixer-Hidden=true";

/// Brings the launcher entries and icons up to date with the running AppImage. Does nothing
/// outside an AppImage.
pub fn refresh() {
    let Some(appimage) = std::env::var_os("APPIMAGE").map(PathBuf::from).filter(|path| path.is_file()) else { return };
    let Some(data_home) = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share")))
    else {
        return;
    };
    let applications = data_home.join("applications");
    let hicolor = data_home.join("icons/hicolor");

    let icon_changed = write_if_changed(&hicolor.join(format!("512x512/apps/{APP_ID}.png")), LOGO);
    let mut entries_changed = false;
    for (path, icon) in entries(&applications, &appimage) {
        if Path::new(&icon).is_absolute() && write_if_changed(Path::new(&icon), LOGO) {
            touch(&path);
            entries_changed = true;
        }
    }
    entries_changed |= write_hidden_entry(&applications.join(format!("{APP_ID}.desktop")), &appimage);

    if icon_changed {
        touch(&hicolor);
        if hicolor.join("icon-theme.cache").exists() {
            run("gtk-update-icon-cache", &["-q", "-t", "-f"], &hicolor);
        }
    }
    if entries_changed {
        run("update-desktop-database", &["-q"], &applications);
        // Plasma's application cache, which KWin finds the launcher for a window in.
        run("kbuildsycoca6", &[], Path::new(""));
    }
}

/// The launchers that start this AppImage or own its windows, with the icon each names.
fn entries(applications: &Path, appimage: &Path) -> Vec<(PathBuf, String)> {
    let appimage = appimage.to_string_lossy();
    let Ok(dir) = std::fs::read_dir(applications) else { return Vec::new() };
    dir.flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|extension| extension == "desktop"))
        .filter_map(|path| {
            let text = std::fs::read_to_string(&path).ok()?;
            let value = |key: &str| text.lines().find_map(|line| line.strip_prefix(key)?.trim_start().strip_prefix('=')).map(str::trim);
            let starts_it = ["Exec", "TryExec"].iter().any(|key| value(key).is_some_and(|exec| exec.contains(appimage.as_ref())));
            let owns_windows = value("StartupWMClass") == Some(APP_ID);
            let icon = value("Icon")?.to_owned();
            (starts_it || owns_windows).then_some((path, icon))
        })
        .collect()
}

/// Writes the hidden launcher named after the app id, unless a launcher of that name that this
/// module didn't write is already there. Whether it wrote.
fn write_hidden_entry(path: &Path, appimage: &Path) -> bool {
    if std::fs::read_to_string(path).is_ok_and(|text| !text.lines().any(|line| line.trim() == OWN_KEY)) {
        return false;
    }
    // Quoted as the desktop entry spec asks, so a path with spaces still runs.
    let mut exec = String::from('"');
    for c in appimage.to_string_lossy().chars() {
        match c {
            '\\' => exec.push_str("\\\\\\\\"),
            '"' | '`' | '$' => {
                exec.push_str("\\\\");
                exec.push(c);
            }
            _ => exec.push(c),
        }
    }
    exec.push('"');
    let text = format!(
        "[Desktop Entry]\nType=Application\nName=Viper Mixer\nExec={exec}\nIcon={APP_ID}\nTerminal=false\nNoDisplay=true\nStartupWMClass={APP_ID}\n{OWN_KEY}\n"
    );
    write_if_changed(path, text.as_bytes())
}

/// Writes `bytes` to `path` unless it already holds them. Whether it wrote.
fn write_if_changed(path: &Path, bytes: &[u8]) -> bool {
    if std::fs::read(path).is_ok_and(|old| old == bytes) {
        return false;
    }
    let written = path.parent().map_or(Ok(()), std::fs::create_dir_all).and_then(|()| std::fs::write(path, bytes));
    match written {
        Ok(()) => true,
        Err(error) => {
            eprintln!("Couldn't write {}: {error}", path.display());
            false
        }
    }
}

/// Sets `path`'s modified time to now, so the caches that read it read it again.
fn touch(path: &Path) {
    if let Err(error) = std::fs::File::open(path).and_then(|file| file.set_modified(std::time::SystemTime::now())) {
        eprintln!("Couldn't mark {} changed: {error}", path.display());
    }
}

/// Runs a desktop cache tool on `target` if it is installed, quietly.
fn run(program: &str, args: &[&str], target: &Path) {
    let mut command = std::process::Command::new(program);
    command.args(args);
    if !target.as_os_str().is_empty() {
        command.arg(target);
    }
    let ran = command.stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).status();
    if let Err(error) = ran
        && error.kind() != std::io::ErrorKind::NotFound
    {
        eprintln!("Couldn't run {program}: {error}");
    }
}
