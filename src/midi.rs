//! MIDI controls: what a hardware fader or button sends, and the worker thread that owns the
//! connection to the controller.

use midir::{Ignore, MidiInput, MidiInputConnection};
use std::sync::mpsc::{self, Receiver, Sender, SyncSender};
use std::thread;

const CLIENT: &str = "Viper Mixer";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    Cc,
    Note,
}

/// A control on the hardware: a CC or a note, on one channel or on any.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Control {
    pub kind: Kind,
    /// 0-based; `None` matches every channel.
    pub channel: Option<u8>,
    pub number: u8,
}

impl Control {
    pub const fn cc(number: u8) -> Self {
        Self { kind: Kind::Cc, channel: None, number }
    }

    pub const fn note(number: u8) -> Self {
        Self { kind: Kind::Note, channel: None, number }
    }

    fn matches(&self, kind: Kind, channel: u8, number: u8) -> bool {
        self.kind == kind && self.number == number && self.channel.is_none_or(|own| own == channel)
    }

    pub fn label(&self) -> String {
        let base = match self.kind {
            Kind::Cc => format!("CC {}", self.number),
            Kind::Note => format!("NOTE {}", self.number),
        };
        match self.channel {
            Some(channel) => format!("{base} · CH {}", channel + 1),
            None => base,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Message {
    Cc { channel: u8, number: u8, value: u8 },
    Note { channel: u8, number: u8, on: bool },
}

impl Message {
    pub fn is(&self, control: &Control) -> bool {
        match *self {
            Self::Cc { channel, number, .. } => control.matches(Kind::Cc, channel, number),
            Self::Note { channel, number, .. } => control.matches(Kind::Note, channel, number),
        }
    }

    /// The control that sent this, bound to its channel.
    pub fn control(&self) -> Control {
        let (kind, channel, number) = match *self {
            Self::Cc { channel, number, .. } => (Kind::Cc, channel, number),
            Self::Note { channel, number, .. } => (Kind::Note, channel, number),
        };
        Control { kind, channel: Some(channel), number }
    }

    /// A button going down: a note on, or a CC at or above the midpoint.
    pub fn pressed(&self) -> bool {
        match *self {
            Self::Cc { value, .. } => value >= 64,
            Self::Note { on, .. } => on,
        }
    }
}

/// Reads one channel message. Everything but control changes and notes (clock, sysex, pitch
/// bend, aftertouch) is ignored, as are messages cut short.
pub(crate) fn parse(data: &[u8]) -> Option<Message> {
    let [status, number, value, ..] = *data else {
        return None;
    };
    if number > 127 || value > 127 {
        return None;
    }
    let channel = status & 0x0f;
    match status & 0xf0 {
        0xb0 => Some(Message::Cc { channel, number, value }),
        // A note on with velocity 0 is a note off, which many controllers send instead.
        0x90 => Some(Message::Note { channel, number, on: value > 0 }),
        0x80 => Some(Message::Note { channel, number, on: false }),
        _ => None,
    }
}

/// ALSA port names end in the client and port numbers ("nanoKONTROL2:CTRL 24:0"), which change
/// when the device is plugged into another USB port or after a reboot. Settings keep the name
/// without them, so the same controller is found again.
pub(crate) fn stable_port_name(name: &str) -> &str {
    let Some((head, tail)) = name.rsplit_once(' ') else {
        return name;
    };
    let numbers = tail.split_once(':').is_some_and(|(client, port)| {
        !client.is_empty()
            && !port.is_empty()
            && client.bytes().all(|b| b.is_ascii_digit())
            && port.bytes().all(|b| b.is_ascii_digit())
    });
    if numbers { head } else { name }
}

/// The port Automatic picks: the first that looks like hardware.
pub(crate) fn preferred_port(ports: &[String]) -> Option<&String> {
    ports
        .iter()
        .find(|port| {
            let lower = port.to_ascii_lowercase();
            !["midi through", "virtual", "dummy", "loopback", "viper mixer"]
                .iter()
                .any(|marker| lower.contains(marker))
        })
        .or_else(|| ports.first())
}

pub(crate) enum Command {
    ListPorts,
    Connect { generation: u64, port: String },
    Disconnect,
}

pub(crate) enum Event {
    Ports(Result<Vec<String>, String>),
    Connected { generation: u64, port: String, result: Result<(), String> },
}

/// Starts the worker. Opening and closing a connection can block inside the MIDI backend, so
/// neither happens on the window's thread.
pub(crate) fn start(
    events: impl Fn(Event) + Send + 'static,
    messages: SyncSender<(u64, Vec<u8>)>,
    wake: impl Fn() + Send + Sync + 'static,
) -> Sender<Command> {
    let (sender, commands) = mpsc::channel();
    thread::Builder::new()
        .name("mixer-midi".into())
        .spawn(move || run(&commands, &events, &messages, std::sync::Arc::new(wake)))
        .expect("could not start the MIDI worker");
    sender
}

fn run(
    commands: &Receiver<Command>,
    events: &impl Fn(Event),
    messages: &SyncSender<(u64, Vec<u8>)>,
    wake: std::sync::Arc<dyn Fn() + Send + Sync>,
) {
    let mut connection: Option<MidiInputConnection<()>> = None;
    while let Ok(command) = commands.recv() {
        match command {
            Command::ListPorts => events(Event::Ports(list_ports())),
            Command::Disconnect => drop(connection.take()),
            Command::Connect { generation, port } => {
                // The old connection closes first, so a device that allows one reader reopens.
                drop(connection.take());
                let result = connect(&port, generation, messages.clone(), wake.clone());
                let result = result.map(|opened| drop(connection.replace(opened)));
                events(Event::Connected { generation, port, result });
            }
        }
    }
}

pub(crate) fn list_ports() -> Result<Vec<String>, String> {
    let input = MidiInput::new(CLIENT).map_err(|error| error.to_string())?;
    Ok(input.ports().iter().filter_map(|port| input.port_name(port).ok()).collect())
}

fn connect(
    port_name: &str,
    generation: u64,
    messages: SyncSender<(u64, Vec<u8>)>,
    wake: std::sync::Arc<dyn Fn() + Send + Sync>,
) -> Result<MidiInputConnection<()>, String> {
    let mut input = MidiInput::new(CLIENT).map_err(|error| error.to_string())?;
    // Sysex, clock and active sensing would only be dropped by `parse`.
    input.ignore(Ignore::All);
    let port = input
        .ports()
        .into_iter()
        .find(|port| input.port_name(port).is_ok_and(|name| name == port_name))
        .ok_or_else(|| "the device went away".to_owned())?;
    input
        .connect(
            &port,
            "viper-mixer-input",
            move |_, message, _| {
                // A full queue means the window is far behind; it catches up on the next fader
                // move, so dropping here is better than blocking the MIDI thread.
                if messages.try_send((generation, message.to_vec())).is_ok() {
                    wake();
                }
            },
            (),
        )
        .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notes_ccs_and_running_garbage() {
        assert_eq!(parse(&[0xb2, 7, 100]), Some(Message::Cc { channel: 2, number: 7, value: 100 }));
        assert_eq!(parse(&[0x90, 37, 0]), Some(Message::Note { channel: 0, number: 37, on: false }));
        assert_eq!(parse(&[0x9f, 37, 1]), Some(Message::Note { channel: 15, number: 37, on: true }));
        assert_eq!(parse(&[0x80, 37, 64]), Some(Message::Note { channel: 0, number: 37, on: false }));
        assert_eq!(parse(&[0xe0, 0, 64]), None, "pitch bend");
        assert_eq!(parse(&[0xb0, 7]), None, "cut short");
        assert_eq!(parse(&[0xf8]), None, "clock");
        assert_eq!(parse(&[0xb0, 200, 1]), None, "data bytes have the top bit clear");
    }

    #[test]
    fn controls_match_their_channel_or_any() {
        let any = Control::cc(1);
        let first = Control { channel: Some(0), ..any };
        let message = Message::Cc { channel: 3, number: 1, value: 0 };
        assert!(message.is(&any));
        assert!(!message.is(&first));
        assert!(!message.is(&Control::note(1)));
        assert_eq!(message.control(), Control { channel: Some(3), ..any });
        assert_eq!(message.control().label(), "CC 1 · CH 4");
    }

    #[test]
    fn port_names_lose_their_alsa_numbers() {
        assert_eq!(stable_port_name("nanoKONTROL2:nanoKONTROL2 _ CTRL 24:0"), "nanoKONTROL2:nanoKONTROL2 _ CTRL");
        assert_eq!(stable_port_name("Midi Through:Midi Through Port-0 14:0"), "Midi Through:Midi Through Port-0");
        assert_eq!(stable_port_name("My Device"), "My Device");
        assert_eq!(stable_port_name("Ratio 3:2"), "Ratio");
        assert_eq!(stable_port_name("Port 1:"), "Port 1:");
    }

    #[test]
    fn automatic_skips_software_ports() {
        let ports = ["Midi Through:Port-0 14:0", "X-Touch:X-Touch MIDI 1 20:0"].map(String::from);
        assert_eq!(preferred_port(&ports), Some(&ports[1]));
        assert_eq!(preferred_port(&ports[..1]), Some(&ports[0]));
    }
}
