//! The worker threads behind the window: PipeWire through `pw-dump` and `wpctl`, and the MIDI
//! worker. Each reply wakes the window, which reads it in `poll`.

use crate::midi;
use crate::stream::{self, Graph};
use std::collections::HashMap;
use std::io::{ErrorKind, Read};
use std::process::{Command as Process, Stdio};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::Duration;

/// `pw-dump` answers in well under a second; one that takes longer is stuck (PipeWire
/// restarting, a wedged client), and the next scan tries again.
const DUMP_TIMEOUT: Duration = Duration::from_secs(5);
const WPCTL_TIMEOUT: Duration = Duration::from_secs(2);

/// A `wpctl` target: a node id, or `@DEFAULT_AUDIO_SINK@` and `@DEFAULT_AUDIO_SOURCE@`.
pub(crate) type Target = String;

pub(crate) enum Command {
    Scan,
    SetVolume(Target, f32),
    SetMute(Target, bool),
    Shutdown,
}

pub(crate) enum Event {
    Graph(Result<Graph, String>),
    Midi(midi::Event),
    CommandFailed(String),
}

pub(crate) struct Engine {
    commands: Sender<Command>,
    midi: Sender<midi::Command>,
    pub events: Receiver<Event>,
    pub messages: Receiver<(u64, Vec<u8>)>,
}

impl Engine {
    pub fn start(wake: rune::Waker) -> Self {
        let (commands, receiver) = mpsc::channel();
        let (events_tx, events) = mpsc::channel();
        let (messages_tx, messages) = mpsc::sync_channel(1024);
        let midi_events = events_tx.clone();
        let midi_wake = wake.clone();
        let midi = midi::start(
            move |event| {
                let _ = midi_events.send(Event::Midi(event));
                midi_wake();
            },
            messages_tx,
            {
                let wake = wake.clone();
                move || wake()
            },
        );
        thread::Builder::new()
            .name("mixer-pipewire".into())
            .spawn(move || run(&receiver, &events_tx, &wake))
            .expect("could not start the PipeWire worker");
        Self { commands, midi, events, messages }
    }

    pub fn send(&self, command: Command) {
        let _ = self.commands.send(command);
    }

    pub fn midi(&self, command: midi::Command) {
        let _ = self.midi.send(command);
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        // Changes already queued go out before the worker stops.
        let _ = self.commands.send(Command::Shutdown);
        let _ = self.midi.send(midi::Command::Disconnect);
    }
}

fn run(commands: &Receiver<Command>, events: &Sender<Event>, wake: &rune::Waker) {
    let send = |event| {
        let _ = events.send(event);
        wake();
    };
    while let Ok(first) = commands.recv() {
        // A fader sweep queues dozens of levels; only the newest per target needs to go out.
        let mut volumes = HashMap::new();
        let mut mutes = HashMap::new();
        let mut shutdown = false;
        for command in std::iter::once(first).chain(commands.try_iter()) {
            match command {
                Command::Scan => {
                    // A slow dump mustn't hold up the faders.
                    let events = events.clone();
                    let wake = Arc::clone(wake);
                    thread::spawn(move || {
                        let _ = events.send(Event::Graph(scan()));
                        wake();
                    });
                }
                Command::SetVolume(target, volume) => {
                    volumes.insert(target, volume);
                }
                Command::SetMute(target, muted) => {
                    mutes.insert(target, muted);
                }
                Command::Shutdown => shutdown = true,
            }
        }
        for (target, volume) in volumes {
            if let Err(error) = run_tool("wpctl", &["set-volume", &target, &format!("{volume:.4}")], WPCTL_TIMEOUT) {
                send(Event::CommandFailed(error));
            }
        }
        for (target, muted) in mutes {
            if let Err(error) = run_tool("wpctl", &["set-mute", &target, if muted { "1" } else { "0" }], WPCTL_TIMEOUT) {
                send(Event::CommandFailed(error));
            }
        }
        if shutdown {
            return;
        }
    }
}

fn scan() -> Result<Graph, String> {
    let output = run_tool("pw-dump", &[], DUMP_TIMEOUT)?;
    let data: Vec<serde_json::Value> =
        serde_json::from_slice(&output).map_err(|error| format!("pw-dump gave unreadable data: {error}"))?;
    Ok(stream::parse(&data))
}

/// Runs a command and returns its output, killing it if it doesn't finish in time.
pub(crate) fn run_tool(program: &str, arguments: &[&str], timeout: Duration) -> Result<Vec<u8>, String> {
    let mut child = Process::new(program)
        .args(arguments)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| match error.kind() {
            ErrorKind::NotFound if program == "pw-dump" || program == "wpctl" => {
                format!("{program} isn't installed; install the PipeWire tools")
            }
            ErrorKind::NotFound => format!("{program} isn't installed"),
            _ => format!("Could not run {program}: {error}"),
        })?;
    // Both pipes are read on their own threads, so a large dump can't fill one and stall.
    let (done, finished) = mpsc::channel();
    let mut stdout = child.stdout.take().expect("stdout is piped");
    let mut stderr = child.stderr.take().expect("stderr is piped");
    let error_reader = thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = stderr.read_to_end(&mut bytes);
        bytes
    });
    thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = stdout.read_to_end(&mut bytes);
        let _ = done.send(bytes);
    });
    let Ok(output) = finished.recv_timeout(timeout) else {
        let _ = child.kill();
        let _ = child.wait();
        return Err(format!("{program} didn't answer within {} s", timeout.as_secs()));
    };
    let status = child.wait().map_err(|error| format!("{program} failed: {error}"))?;
    if status.success() {
        return Ok(output);
    }
    let stderr = error_reader.join().unwrap_or_default();
    Err(format!(
        "{program} {} failed: {}",
        arguments.join(" "),
        String::from_utf8_lossy(&stderr).trim()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_tool_says_so() {
        let error = run_tool("viper-mixer-no-such-tool", &[], WPCTL_TIMEOUT).unwrap_err();
        assert!(error.contains("isn't installed"), "{error}");
    }

    #[test]
    fn a_stuck_tool_is_killed() {
        let error = run_tool("sleep", &["5"], Duration::from_millis(100)).unwrap_err();
        assert!(error.contains("didn't answer"), "{error}");
    }

    #[test]
    fn output_and_failures_come_back() {
        assert_eq!(run_tool("printf", &["hi"], WPCTL_TIMEOUT).unwrap(), b"hi");
        let error = run_tool("sh", &["-c", "echo nope >&2; exit 3"], WPCTL_TIMEOUT).unwrap_err();
        assert!(error.ends_with("failed: nope"), "{error}");
    }
}
