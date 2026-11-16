//! How the workers, the tray and a second launch reach the main thread: they wake it, ask it to
//! show the window, or ask it to quit. The main thread's event loop sets how it gets woken.

use std::sync::{Arc, Mutex};

/// Wakes the main thread to read what a worker sent.
pub(crate) type Waker = Arc<dyn Fn() + Send + Sync>;

/// What was asked for since the main thread last looked.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Pending {
    pub show: bool,
    pub quit: bool,
}

#[derive(Clone, Default)]
pub(crate) struct Signal(Arc<Shared>);

#[derive(Default)]
struct Shared {
    pending: Mutex<Pending>,
    /// Wakes the event loop; `None` before it starts and after it ends.
    wake: Mutex<Option<Waker>>,
}

impl Signal {
    pub fn waker(&self) -> Waker {
        let signal = self.clone();
        Arc::new(move || signal.raise(|_| {}))
    }

    pub fn show(&self) {
        self.raise(|pending| pending.show = true);
    }

    pub fn quit(&self) {
        self.raise(|pending| pending.quit = true);
    }

    fn raise(&self, ask: impl FnOnce(&mut Pending)) {
        ask(&mut self.0.pending.lock().unwrap_or_else(|e| e.into_inner()));
        let wake = self.0.wake.lock().unwrap_or_else(|e| e.into_inner()).clone();
        if let Some(wake) = wake {
            wake();
        }
    }

    /// Sets how the main thread gets woken each time something is raised.
    pub fn set_waker(&self, wake: Option<Waker>) {
        *self.0.wake.lock().unwrap_or_else(|e| e.into_inner()) = wake;
    }

    /// What was asked for, clearing it.
    pub fn take(&self) -> Pending {
        std::mem::take(&mut self.0.pending.lock().unwrap_or_else(|e| e.into_inner()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;

    #[test]
    fn a_show_from_another_thread_wakes_the_main_thread() {
        let signal = Signal::default();
        let (woken, wait) = mpsc::channel();
        let woken = Mutex::new(woken);
        signal.set_waker(Some(Arc::new(move || {
            let _ = woken.lock().unwrap().send(());
        })));
        let other = signal.clone();
        std::thread::spawn(move || other.show());
        wait.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(signal.take(), Pending { show: true, quit: false });
        assert_eq!(signal.take(), Pending::default());
    }
}
