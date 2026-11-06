//! How the workers, the tray and a second launch reach the main thread: they wake it, ask it to
//! show the window, or ask it to quit. The main thread waits on it while the window is closed to
//! the tray, and the window listens to it while it's open.

use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

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
    pending: Mutex<(bool, Pending)>,
    raised: Condvar,
    window: Mutex<Option<UnboundedSender<()>>>,
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
        let mut state = self.0.pending.lock().unwrap_or_else(|e| e.into_inner());
        state.0 = true;
        ask(&mut state.1);
        self.0.raised.notify_all();
        if let Some(window) = &*self.0.window.lock().unwrap_or_else(|e| e.into_inner()) {
            let _ = window.unbounded_send(());
        }
    }

    /// What was asked for, clearing it.
    pub fn take(&self) -> Pending {
        let mut state = self.0.pending.lock().unwrap_or_else(|e| e.into_inner());
        state.0 = false;
        std::mem::take(&mut state.1)
    }

    /// Waits until something is raised or `timeout` passes, then takes what was asked for.
    pub fn wait(&self, timeout: Duration) -> Pending {
        let state = self.0.pending.lock().unwrap_or_else(|e| e.into_inner());
        let (mut state, _) = self.0.raised.wait_timeout_while(state, timeout, |state| !state.0).unwrap_or_else(|e| e.into_inner());
        state.0 = false;
        std::mem::take(&mut state.1)
    }

    /// Sends the open window a message each time something is raised, until `close_window`.
    pub fn open_window(&self) -> UnboundedReceiver<()> {
        let (sender, receiver) = unbounded();
        *self.0.window.lock().unwrap_or_else(|e| e.into_inner()) = Some(sender);
        receiver
    }

    pub fn close_window(&self) {
        *self.0.window.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_show_from_another_thread_ends_the_wait() {
        let signal = Signal::default();
        let other = signal.clone();
        std::thread::spawn(move || other.show());
        assert_eq!(signal.wait(Duration::from_secs(5)), Pending { show: true, quit: false });
        assert_eq!(signal.take(), Pending::default());
    }
}
