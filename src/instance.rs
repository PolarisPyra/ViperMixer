//! One Viper Mixer per user: the first launch holds a lock and listens on a socket, and a later
//! launch asks it to show its window, then exits.

use std::fs::File;
use std::io;
use std::path::PathBuf;

/// Held for the life of the process; the lock goes with it, even after a crash.
pub(crate) struct Instance(#[allow(dead_code)] File);

fn runtime_dir() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR")
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
}

/// Takes the per-user lock, or returns `None` when another Viper Mixer holds it.
pub(crate) fn acquire() -> io::Result<Option<Instance>> {
    let file = File::options()
        .create(true)
        .truncate(false)
        .write(true)
        .open(runtime_dir().join("viper-mixer.lock"))?;
    match file.try_lock() {
        Ok(()) => Ok(Some(Instance(file))),
        Err(std::fs::TryLockError::WouldBlock) => Ok(None),
        Err(std::fs::TryLockError::Error(error)) => Err(error),
    }
}

#[cfg(unix)]
mod socket {
    use std::io::{self, Read, Write};
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::path::PathBuf;
    use std::time::Duration;

    fn path() -> PathBuf {
        super::runtime_dir().join("viper-mixer.sock")
    }

    /// Asks the running instance to show its window, passing on the launcher's activation
    /// token so the compositor lets that window take focus.
    pub(crate) fn show_running() -> io::Result<()> {
        let mut stream = UnixStream::connect(path())?;
        let token = ["XDG_ACTIVATION_TOKEN", "DESKTOP_STARTUP_ID"].iter().find_map(|name| std::env::var(name).ok());
        stream.write_all(token.unwrap_or_default().as_bytes())
    }

    /// Calls `on_launch` from a background thread for each later launch, with its activation
    /// token if it had one. Only call it while holding the lock, since it replaces a stale
    /// socket.
    pub(crate) fn listen(on_launch: impl Fn(Option<String>) + Send + 'static) {
        let path = path();
        let _ = std::fs::remove_file(&path);
        let listener = match UnixListener::bind(&path) {
            Ok(listener) => listener,
            Err(error) => {
                eprintln!("Can't listen for other launches: {error}");
                return;
            }
        };
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { continue };
                // A token is a few dozen bytes; a launch that sends nothing still shows the window.
                let _ = stream.set_read_timeout(Some(Duration::from_millis(500)));
                let mut token = String::new();
                let _ = stream.take(4096).read_to_string(&mut token);
                let token = token.trim();
                on_launch((!token.is_empty()).then(|| token.to_owned()));
            }
        });
    }
}

#[cfg(not(unix))]
mod socket {
    pub(crate) fn show_running() -> std::io::Result<()> {
        Ok(())
    }

    pub(crate) fn listen(_on_launch: impl Fn(Option<String>) + Send + 'static) {}
}

pub(crate) use socket::{listen, show_running};
