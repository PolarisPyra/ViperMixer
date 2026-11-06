//! Keeps Viper Mixer up to date from its GitHub releases, the way Viper Music does. Shortly after
//! launch and then every hour it asks GitHub for the latest release. A copy running from a release
//! AppImage downloads the new AppImage beside itself, checks it against the SHA-256 GitHub
//! publishes, and moves it into place; the new version runs from the next start, and the window
//! offers a restart. A source build can't replace itself, so it only says a release is out.
//! `curl` and `sha256sum` do the network and the checksum, as `pw-dump` and `wpctl` do PipeWire,
//! so the app needs no HTTP or TLS library.

use crate::engine::run_tool;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant, SystemTime};
use std::{fs, thread};

/// The release page Download opens, for copies that can't install updates.
pub(crate) const RELEASES_URL: &str = "https://github.com/PolarisPyra/ViperMixer/releases/latest";
const LATEST_RELEASE_API: &str = "https://api.github.com/repos/PolarisPyra/ViperMixer/releases/latest";
const USER_AGENT: &str = concat!("ViperMixer/", env!("CARGO_PKG_VERSION"));
/// How long after launch the first check waits, so it doesn't slow the start.
const FIRST_CHECK: Duration = Duration::from_secs(30);
/// How often to check. Measured on the wall clock, so time spent asleep counts.
const CHECK_EVERY: Duration = Duration::from_secs(60 * 60);
/// How often the timer looks at the clock.
const TICK: Duration = Duration::from_secs(5 * 60);
const CHECK_TIMEOUT: Duration = Duration::from_secs(30);
/// A download that hasn't finished by then has stalled; the next check starts it again.
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(30 * 60);

/// The AppImage the app starts again into once it has quit, set by [`Updater::restart`].
static RESTART_INTO: OnceLock<PathBuf> = OnceLock::new();

/// Where updating stands.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) enum UpdateState {
    /// No check has finished yet.
    #[default]
    Idle,
    /// A check the user asked for is running.
    Checking,
    /// The latest release is the running version.
    UpToDate,
    /// A newer release this copy can't install itself.
    Available { version: String },
    /// The newer release is downloading; `percent` is unknown when GitHub doesn't give a size.
    Downloading { version: String, percent: Option<u8> },
    /// Installed; it runs once Viper Mixer restarts.
    Ready { version: String },
    /// The last check the user asked for, or the last download, failed with this message.
    Failed { message: String },
}

#[derive(Default)]
struct Shared {
    state: Mutex<UpdateState>,
    /// Set on each change, for the window's `poll`.
    changed: AtomicBool,
    /// One check or download at a time.
    busy: AtomicBool,
    last_check: Mutex<Option<SystemTime>>,
    /// Wakes the window when the state changes; set by [`Updater::start`].
    wake: OnceLock<rune::Waker>,
}

/// Checks for and installs updates on background threads; the window reads its state.
#[derive(Clone, Default)]
pub(crate) struct Updater {
    shared: Arc<Shared>,
}

/// The parts of GitHub's latest release the updater uses.
#[derive(Debug, PartialEq, Eq)]
struct Release {
    version: [u32; 3],
    appimage: Option<Asset>,
}

#[derive(Debug, PartialEq, Eq)]
struct Asset {
    url: String,
    /// The hex SHA-256 GitHub publishes for the asset, when it does.
    sha256: Option<String>,
    size: u64,
}

/// The AppImage this copy runs from, when it is a release AppImage that can replace itself.
fn installed_appimage() -> Option<PathBuf> {
    if cfg!(debug_assertions) {
        return None;
    }
    std::env::var_os("APPIMAGE").map(PathBuf::from).filter(|path| path.is_file())
}

impl Updater {
    /// Starts the background checks; `wake` wakes the window when the state changes.
    pub fn start(wake: rune::Waker) -> Self {
        let updater = Self::default();
        let _ = updater.shared.wake.set(wake);
        let shared = Arc::clone(&updater.shared);
        let spawned = thread::Builder::new().name("updates".into()).spawn(move || {
            thread::sleep(FIRST_CHECK);
            loop {
                if shared.due() {
                    shared.check(false);
                }
                thread::sleep(TICK);
            }
        });
        if let Err(error) = spawned {
            eprintln!("Update checks couldn't start: {error}");
        }
        updater
    }

    pub fn state(&self) -> UpdateState {
        self.shared.state()
    }

    /// Shows `state` without checking, for the demo scenes.
    pub fn show(&self, state: UpdateState) {
        self.shared.set(state);
    }

    /// Whether the state changed since the last call.
    pub fn take_changed(&self) -> bool {
        self.shared.changed.swap(false, Ordering::SeqCst)
    }

    /// Checks for a newer release on a background thread, showing its progress and errors.
    pub fn check_now(&self) {
        let shared = Arc::clone(&self.shared);
        if let Err(error) = thread::Builder::new().name("update-check".into()).spawn(move || shared.check(true)) {
            self.shared.set(UpdateState::Failed { message: format!("Couldn't check for updates: {error}") });
        }
    }

    /// Remembers the installed update so `main` starts it once the app has quit. Returns whether
    /// an update is installed and the app should now quit.
    pub fn restart(&self) -> bool {
        if !matches!(self.shared.state(), UpdateState::Ready { .. }) {
            return false;
        }
        installed_appimage().is_some_and(|path| {
            let _ = RESTART_INTO.set(path);
            true
        })
    }
}

/// The installed update to start once the app has quit, if the user asked to restart into it.
pub(crate) fn restart_target() -> Option<&'static Path> {
    RESTART_INTO.get().map(PathBuf::as_path)
}

impl Shared {
    fn state(&self) -> UpdateState {
        self.state.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    fn set(&self, state: UpdateState) {
        *self.state.lock().unwrap_or_else(PoisonError::into_inner) = state;
        self.changed.store(true, Ordering::SeqCst);
        if let Some(wake) = self.wake.get() {
            wake();
        }
    }

    /// Whether an hour has passed since the last check.
    fn due(&self) -> bool {
        self.last_check
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .is_none_or(|last| last.elapsed().map_or(true, |since| since >= CHECK_EVERY))
    }

    /// Checks for a newer release, then downloads and installs it. A check the user asked for
    /// (`watched`) shows its progress and its errors; a background one changes the state only when
    /// it finds something, so going offline doesn't hide an update already found.
    fn check(&self, watched: bool) {
        if self.busy.swap(true, Ordering::SeqCst) {
            return;
        }
        *self.last_check.lock().unwrap_or_else(PoisonError::into_inner) = Some(SystemTime::now());
        // An installed update waits for the restart; checking again would download it again.
        if !matches!(self.state(), UpdateState::Ready { .. }) {
            if watched {
                self.set(UpdateState::Checking);
            }
            if let Err(message) = self.update(watched) {
                eprintln!("Update failed: {message}");
                let shown = self.state();
                if watched || matches!(shown, UpdateState::Downloading { .. }) {
                    self.set(UpdateState::Failed { message });
                } else if shown == UpdateState::Checking {
                    self.set(UpdateState::Idle);
                }
            }
        }
        self.busy.store(false, Ordering::SeqCst);
    }

    fn update(&self, watched: bool) -> Result<(), String> {
        let body = run_tool(
            "curl",
            &["-fsSL", "--max-time", "30", "-A", USER_AGENT, "-H", "Accept: application/vnd.github+json", LATEST_RELEASE_API],
            CHECK_TIMEOUT + Duration::from_secs(5),
        )
        .map_err(|error| format!("Couldn't reach GitHub: {}", curl_reason(&error)))?;
        let release = parse_release(&body)?;
        let current = parse_version(env!("CARGO_PKG_VERSION")).ok_or("This build has no release version")?;
        if release.version <= current {
            if watched || matches!(self.state(), UpdateState::Idle | UpdateState::Failed { .. }) {
                self.set(UpdateState::UpToDate);
            }
            return Ok(());
        }
        let version = release.version.map(|part| part.to_string()).join(".");
        let Some(target) = installed_appimage() else {
            self.set(UpdateState::Available { version });
            return Ok(());
        };
        let asset = release.appimage.ok_or("The release has no AppImage")?;
        let sha256 = asset.sha256.ok_or("The release doesn't publish the AppImage's checksum")?;
        self.download(&asset.url, &sha256, asset.size, &target, &version)?;
        self.set(UpdateState::Ready { version });
        Ok(())
    }

    /// Downloads the AppImage beside `target`, checks its SHA-256, and moves it over `target`.
    /// The running copy keeps its open file, so it mixes on until the restart.
    fn download(&self, url: &str, sha256: &str, size: u64, target: &Path, version: &str) -> Result<(), String> {
        let partial = target.with_extension("download");
        let result = (|| {
            let progress = |percent| UpdateState::Downloading { version: version.to_owned(), percent };
            let percent = |path: &Path| (size > 0).then(|| fs::metadata(path).map_or(0, |m| (m.len() * 100 / size).min(100) as u8));
            let mut child = Command::new("curl")
                .args(["-fsSL", "--max-time", &DOWNLOAD_TIMEOUT.as_secs().to_string(), "-A", USER_AGENT, "-o"])
                .arg(&partial)
                .arg(url)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .spawn()
                .map_err(|error| format!("Couldn't run curl: {error}"))?;
            let started = Instant::now();
            let mut shown = None;
            self.set(progress(percent(&partial)));
            let status = loop {
                if let Some(status) = child.try_wait().map_err(|error| format!("The download stopped: {error}"))? {
                    break status;
                }
                if started.elapsed() > DOWNLOAD_TIMEOUT + Duration::from_secs(60) {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err("The download stalled".to_owned());
                }
                let now = percent(&partial);
                if now != shown {
                    shown = now;
                    self.set(progress(now));
                }
                thread::sleep(Duration::from_millis(250));
            };
            if !status.success() {
                let mut error = String::new();
                if let Some(mut stderr) = child.stderr.take() {
                    let _ = stderr.read_to_string(&mut error);
                }
                return Err(format!("The download stopped: {}", curl_reason(&error)));
            }
            let path = partial.to_str().ok_or("The AppImage's folder has a name that isn't text")?;
            let sums = run_tool("sha256sum", &[path], Duration::from_secs(60))?;
            let got = String::from_utf8_lossy(&sums).split_whitespace().next().unwrap_or_default().to_owned();
            if !got.eq_ignore_ascii_case(sha256) {
                return Err("The downloaded AppImage doesn't match its checksum".to_owned());
            }
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&partial, fs::Permissions::from_mode(0o755))
                .and_then(|()| fs::rename(&partial, target))
                .map_err(|error| format!("Couldn't replace {}: {error}", target.display()))
        })();
        if result.is_err() {
            let _ = fs::remove_file(&partial);
        }
        result
    }
}

/// `YY.M.N`, with or without a leading `v`. Older `0.5.3` tags read the same way and sort below.
fn parse_version(text: &str) -> Option<[u32; 3]> {
    let mut parts = text.trim_start_matches('v').split('.').map(|part| part.parse().ok());
    let version = [parts.next()??, parts.next()??, parts.next()??];
    parts.next().is_none().then_some(version)
}

fn parse_release(body: &[u8]) -> Result<Release, String> {
    let release: serde_json::Value = serde_json::from_slice(body).map_err(|_| "GitHub sent an unreadable release")?;
    let tag = release["tag_name"].as_str().unwrap_or_default();
    let version = parse_version(tag).ok_or_else(|| format!("Unrecognized release tag {tag:?}"))?;
    let appimage = release["assets"].as_array().into_iter().flatten().find_map(|asset| {
        asset["name"].as_str()?.ends_with(".AppImage").then(|| Asset {
            url: asset["browser_download_url"].as_str().unwrap_or_default().to_owned(),
            sha256: asset["digest"].as_str().and_then(|digest| digest.strip_prefix("sha256:")).map(str::to_owned),
            size: asset["size"].as_u64().unwrap_or(0),
        })
    });
    Ok(Release { version, appimage })
}

/// curl's own words from a failed run, without the command line: "Could not resolve host: …".
fn curl_reason(error: &str) -> String {
    match error.rsplit_once("curl: (").and_then(|(_, rest)| rest.split_once(") ")) {
        Some((_, reason)) => reason.trim().to_owned(),
        None => error.trim().to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare_as_numbers() {
        assert_eq!(parse_version("v26.11.0"), Some([26, 11, 0]));
        assert!(parse_version("26.11.10") > parse_version("26.11.9"));
        assert!(parse_version("v0.5.3") < parse_version("26.11.0"));
        assert_eq!(parse_version("26.11"), None);
        assert_eq!(parse_version("26.11.0.1"), None);
        assert_eq!(parse_version("26.11.0-rc1"), None);
    }

    #[test]
    fn a_release_gives_its_appimage_and_checksum() {
        let body = br#"{"tag_name": "v26.12.0", "assets": [
            {"name": "notes.txt", "browser_download_url": "https://x/notes.txt", "size": 3},
            {"name": "Viper-Mixer-x86_64.AppImage", "browser_download_url": "https://x/a.AppImage",
             "digest": "sha256:abc123", "size": 9000}
        ]}"#;
        let release = parse_release(body).unwrap();
        assert_eq!(release.version, [26, 12, 0]);
        let asset = release.appimage.unwrap();
        assert_eq!((asset.url.as_str(), asset.sha256.as_deref(), asset.size), ("https://x/a.AppImage", Some("abc123"), 9000));
        assert!(parse_release(br#"{"tag_name": "nightly", "assets": []}"#).is_err());
    }

    /// A download from a local file: `curl` reads `file://` the same way it reads `https://`.
    fn download_case(name: &str, checksum: impl FnOnce(&str) -> String) -> (Result<(), String>, PathBuf, Shared) {
        let dir = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/target/test-updates")).join(name);
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let release = dir.join("new.AppImage");
        fs::write(&release, "the new version").unwrap();
        let target = dir.join("Viper-Mixer.AppImage");
        fs::write(&target, "the running version").unwrap();
        let sums = run_tool("sha256sum", &[release.to_str().unwrap()], Duration::from_secs(10)).unwrap();
        let sha256 = checksum(String::from_utf8_lossy(&sums).split_whitespace().next().unwrap());
        let shared = Shared::default();
        let url = format!("file://{}", release.display());
        let result = shared.download(&url, &sha256, 15, &target, "26.12.0");
        (result, target, shared)
    }

    #[test]
    fn a_download_replaces_the_appimage() {
        let (result, target, shared) = download_case("good", str::to_owned);
        assert_eq!(result, Ok(()));
        assert_eq!(fs::read_to_string(&target).unwrap(), "the new version");
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(fs::metadata(&target).unwrap().permissions().mode() & 0o777, 0o755);
        assert!(!target.with_extension("download").exists());
        assert!(matches!(shared.state(), UpdateState::Downloading { percent: Some(_), .. }));
    }

    #[test]
    fn a_bad_checksum_keeps_the_running_appimage() {
        let (result, target, _) = download_case("bad", |_| "0".repeat(64));
        assert_eq!(result, Err("The downloaded AppImage doesn't match its checksum".into()));
        assert_eq!(fs::read_to_string(&target).unwrap(), "the running version");
        assert!(!target.with_extension("download").exists());
    }

    #[test]
    fn curl_errors_keep_only_the_reason() {
        let error = "curl -fsSL https://x failed: curl: (6) Could not resolve host: api.github.com";
        assert_eq!(curl_reason(error), "Could not resolve host: api.github.com");
        assert_eq!(curl_reason("curl isn't installed"), "curl isn't installed");
    }
}
