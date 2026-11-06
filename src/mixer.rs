//! The mixer's state and what changes it: PipeWire scans, the MIDI controller, and the strips.
//! The view in `ui` reads it and calls back in.

use crate::config::{Config, FADER_COUNT, INPUT, MIDI_DISCONNECTED, MuteMode, OUTPUT, STEAM_ALL};
use crate::engine::{Command, Engine, Event, Target};
use crate::midi::{self, Message};
use crate::stream::{self, AudioStream, Graph};
use crate::updates::Updater;
use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

const SCAN_EVERY: Duration = Duration::from_secs(2);
const PORTS_EVERY: Duration = Duration::from_secs(3);
const RECONNECT_EVERY: Duration = Duration::from_secs(2);
/// A scan with no answer by now is given up on, so the next one can start.
const SCAN_GIVE_UP: Duration = Duration::from_secs(10);
const SAVE_DELAY: Duration = Duration::from_millis(350);
const OUTPUT_TARGET: &str = "@DEFAULT_AUDIO_SINK@";
const INPUT_TARGET: &str = "@DEFAULT_AUDIO_SOURCE@";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Learn {
    Fader(usize),
    Mute(usize),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Menu {
    Assign(usize),
    Midi,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Health {
    Good,
    Waiting,
    Bad,
}

pub(crate) struct Mixer {
    pub config: Config,
    pub graph: Graph,
    engine: Option<Engine>,
    /// Full port names, as the MIDI backend lists them.
    pub midi_ports: Vec<String>,
    pub connected_port: String,
    midi_generation: u64,
    midi_connecting: bool,
    pub midi_status: String,
    pub pipewire_status: String,
    pub pipewire_health: Health,
    pub muted: [bool; FADER_COUNT],
    /// A strip's mute is only sent once someone touched it, so launching doesn't unmute what
    /// another app muted.
    mute_touched: [bool; FADER_COUNT],
    pub learning: Option<Learn>,
    pub menu: Option<Menu>,
    /// When the controller last moved each strip, for its activity light.
    pub touched_at: [Option<Instant>; FADER_COUNT],
    pub tray_available: bool,
    pub error: Option<String>,
    pub updates: Updater,
    /// The release whose update banner the user closed this session.
    pub update_dismissed: Option<String>,
    last_scan: Option<Instant>,
    last_ports: Option<Instant>,
    last_connect: Option<Instant>,
    scanning: bool,
    listing_ports: bool,
    save_at: Option<Instant>,
    /// What each target was last sent: volume in ten-thousandths, and mute once touched.
    sent: HashMap<Target, (u32, Option<bool>)>,
}

impl Mixer {
    pub fn new(config: Config) -> Self {
        Self {
            config,
            graph: Graph::default(),
            engine: None,
            midi_ports: Vec::new(),
            connected_port: String::new(),
            midi_generation: 0,
            midi_connecting: false,
            midi_status: "Looking for MIDI devices…".into(),
            pipewire_status: "Connecting to PipeWire…".into(),
            pipewire_health: Health::Waiting,
            muted: [false; FADER_COUNT],
            mute_touched: [false; FADER_COUNT],
            learning: None,
            menu: None,
            touched_at: [None; FADER_COUNT],
            tray_available: false,
            error: None,
            updates: Updater::default(),
            update_dismissed: None,
            last_scan: None,
            last_ports: None,
            last_connect: None,
            scanning: false,
            listing_ports: false,
            save_at: None,
            sent: HashMap::new(),
        }
    }

    pub fn attach(&mut self, engine: Engine) {
        self.engine = Some(engine);
        self.tick(Instant::now());
    }

    fn send(&self, command: Command) {
        if let Some(engine) = &self.engine {
            engine.send(command);
        }
    }

    fn midi(&self, command: midi::Command) {
        if let Some(engine) = &self.engine {
            engine.midi(command);
        }
    }

    pub fn schedule_save(&mut self) {
        self.save_at = Some(Instant::now() + SAVE_DELAY);
    }

    pub fn save_now(&mut self) {
        self.save_at = None;
        if let Err(error) = self.config.save() {
            self.error = Some(format!("Couldn't save settings: {error}"));
        }
    }

    // Periodic work ---------------------------------------------------------------------------

    /// Starts scans and reconnects that are due, and saves settings that are. Returns when it
    /// next needs to run.
    pub fn tick(&mut self, now: Instant) -> Duration {
        if self.engine.is_none() {
            return Duration::from_secs(3600);
        }
        let due = |last: Option<Instant>, every| last.is_none_or(|last| now.duration_since(last) >= every);
        if self.scanning && due(self.last_scan, SCAN_GIVE_UP) {
            self.scanning = false;
        }
        if !self.scanning && due(self.last_scan, SCAN_EVERY) {
            self.scanning = true;
            self.last_scan = Some(now);
            self.send(Command::Scan);
        }
        if !self.listing_ports && due(self.last_ports, PORTS_EVERY) {
            self.listing_ports = true;
            self.last_ports = Some(now);
            self.midi(midi::Command::ListPorts);
        }
        if self.wants_midi() && due(self.last_connect, RECONNECT_EVERY) {
            self.connect_midi(now);
        }
        if self.save_at.is_some_and(|at| now >= at) {
            self.save_now();
        }
        let mut next = SCAN_EVERY.min(PORTS_EVERY);
        if let Some(at) = self.save_at {
            next = next.min(at.saturating_duration_since(now));
        }
        next
    }

    /// Takes in what the workers sent. True when anything changed.
    pub fn drain(&mut self, now: Instant) -> bool {
        let mut events = Vec::new();
        let mut messages = Vec::new();
        if let Some(engine) = &self.engine {
            events.extend(engine.events.try_iter());
            messages.extend(engine.messages.try_iter());
        }
        let changed = !events.is_empty() || !messages.is_empty();
        for event in events {
            match event {
                Event::Graph(result) => self.take_graph(result),
                Event::Midi(midi::Event::Ports(result)) => self.take_ports(result, now),
                Event::Midi(midi::Event::Connected { generation, port, result }) => {
                    self.take_connection(generation, port, result);
                }
                Event::CommandFailed(error) => {
                    // Whatever didn't go through is sent again on the next change or scan.
                    self.sent.clear();
                    self.error = Some(error);
                }
            }
        }
        for (generation, data) in messages {
            if generation == self.midi_generation {
                self.handle_midi(&data, now);
            }
        }
        changed
    }

    // PipeWire --------------------------------------------------------------------------------

    pub fn rescan(&mut self) {
        self.last_scan = None;
        self.scanning = false;
        self.tick(Instant::now());
    }

    fn take_graph(&mut self, result: Result<Graph, String>) {
        self.scanning = false;
        let graph = match result {
            Ok(graph) => graph,
            Err(error) => {
                self.pipewire_health = Health::Bad;
                self.pipewire_status = format!("PipeWire unavailable · {error}");
                return;
            }
        };
        // A node id that now belongs to another app, or is gone, starts over.
        let previous: HashMap<_, _> = self.graph.streams.iter().map(|s| (s.node_id.to_string(), &s.identity)).collect();
        let current: HashMap<_, _> = graph.streams.iter().map(|s| (s.node_id.to_string(), &s.identity)).collect();
        let output_changed = self.graph.output != graph.output;
        let input_changed = self.graph.input != graph.input;
        self.sent.retain(|target, _| match target.as_str() {
            // A new default device (headphones plugged in) gets the fader's level too.
            OUTPUT_TARGET => !output_changed,
            INPUT_TARGET => !input_changed,
            id => current.get(id).is_some_and(|identity| previous.get(id) == Some(identity)),
        });
        self.graph = graph;
        // Follow an app whose stream came back under a new name or id.
        let mut renamed = false;
        for index in 0..FADER_COUNT {
            if let Some((identity, name)) = self.assigned_stream(index).map(|s| (s.identity.clone(), s.name.clone()))
                && (self.config.stream_keys[index] != identity || self.config.applications[index] != name)
            {
                self.config.stream_keys[index] = identity;
                self.config.applications[index] = name;
                renamed = true;
            }
        }
        if renamed {
            self.schedule_save();
        }
        let count = self.graph.streams.len();
        self.pipewire_health = Health::Good;
        self.pipewire_status = format!("{count} app{} playing through PipeWire", if count == 1 { "" } else { "s" });
        self.sync_all();
    }

    /// What strip `index` is assigned to, as its menu id: "", a special, or a stream identity
    /// (or saved name, for an assignment from before identities).
    pub fn assignment(&self, index: usize) -> &str {
        let name = &self.config.applications[index];
        if [STEAM_ALL, OUTPUT, INPUT].contains(&name.as_str()) || self.config.stream_keys[index].is_empty() {
            name
        } else {
            &self.config.stream_keys[index]
        }
    }

    fn is_special(&self, index: usize) -> bool {
        [STEAM_ALL, OUTPUT, INPUT].contains(&self.config.applications[index].as_str())
    }

    pub fn assigned_stream(&self, index: usize) -> Option<&AudioStream> {
        if self.is_special(index) {
            return None;
        }
        self.assigned_streams(index).into_iter().next()
    }

    /// An app can own several nodes (one per browser tab), all with one identity, so a strip
    /// drives every one.
    pub fn assigned_streams(&self, index: usize) -> Vec<&AudioStream> {
        let name = self.config.applications[index].as_str();
        let streams = self.graph.streams.iter();
        match name {
            STEAM_ALL => return streams.filter(|s| s.is_steam_game).collect(),
            OUTPUT | INPUT | "" => return Vec::new(),
            _ => {}
        }
        let identity = &self.config.stream_keys[index];
        if !identity.is_empty() {
            let matches: Vec<_> = streams.clone().filter(|s| s.identity == *identity).collect();
            if !matches.is_empty() {
                return matches;
            }
        }
        // Assignments saved before ALSA names were shortened still match their program.
        let name = stream::strip_alsa_wrapper(name);
        streams.filter(|s| s.name == name).collect()
    }

    fn targets(&self, index: usize) -> Vec<Target> {
        match self.config.applications[index].as_str() {
            OUTPUT if self.graph.output.is_some() => vec![OUTPUT_TARGET.into()],
            INPUT if self.graph.input.is_some() => vec![INPUT_TARGET.into()],
            _ => self.assigned_streams(index).iter().map(|s| s.node_id.to_string()).collect(),
        }
    }

    /// Whether something the strip drives is making sound.
    pub fn is_live(&self, index: usize) -> bool {
        match self.config.applications[index].as_str() {
            OUTPUT => self.graph.output.as_ref().is_some_and(|device| device.running),
            INPUT => self.graph.input.as_ref().is_some_and(|device| device.running),
            _ => self.assigned_streams(index).iter().any(|s| s.running),
        }
    }

    /// Whether the strip's assignment is present right now.
    pub fn is_present(&self, index: usize) -> bool {
        match self.config.applications[index].as_str() {
            "" => false,
            STEAM_ALL => true,
            _ => !self.targets(index).is_empty(),
        }
    }

    /// Sends the strip's level and mute to everything it drives, skipping what's already there.
    fn apply(&mut self, index: usize) {
        let volume = self.config.fader_volumes[index];
        let level = (volume * 10_000.0).round() as u32;
        let mute = self.mute_touched[index].then_some(self.muted[index]);
        for target in self.targets(index) {
            let last = self.sent.get(&target).copied();
            if last.is_none_or(|(sent, _)| sent != level) {
                self.send(Command::SetVolume(target.clone(), volume));
            }
            if let Some(muted) = mute
                && last.is_none_or(|(_, sent)| sent != mute)
            {
                self.send(Command::SetMute(target.clone(), muted));
            }
            self.sent.insert(target, (level, mute));
        }
    }

    fn sync_all(&mut self) {
        for index in 0..FADER_COUNT {
            self.apply(index);
        }
    }

    /// Assigns strip `index` to a menu id from `assignment_options`.
    pub fn assign(&mut self, index: usize, choice: &str) {
        if choice == self.assignment(index) {
            return;
        }
        let before = self.targets(index);
        match choice {
            "" | STEAM_ALL | OUTPUT | INPUT => {
                self.config.applications[index] = choice.to_owned();
                self.config.stream_keys[index].clear();
            }
            identity => {
                let Some(stream) = self.graph.streams.iter().find(|s| s.identity == identity) else {
                    return;
                };
                self.config.applications[index] = stream.name.clone();
                self.config.stream_keys[index] = identity.to_owned();
            }
        }
        // Moving a muted strip mustn't leave the app it had muted with no strip to unmute it.
        if self.mute_touched[index] && self.muted[index] {
            let after = self.targets(index);
            for target in before.into_iter().filter(|target| !after.contains(target)) {
                self.send(Command::SetMute(target.clone(), false));
                self.sent.remove(&target);
            }
        }
        for target in self.targets(index) {
            self.sent.remove(&target);
        }
        self.apply(index);
        self.schedule_save();
    }

    /// The assignment menu: id, label, and whether it's playing. The saved app comes last,
    /// marked inactive, while it isn't running.
    pub fn assignment_options(&self, index: usize) -> Vec<(String, String, bool)> {
        let device = |device: &Option<stream::Device>, fallback: &str| {
            device.as_ref().map_or_else(|| fallback.to_owned(), |device| device.description.clone())
        };
        let mut options = vec![
            (String::new(), "Nothing".to_owned(), false),
            (OUTPUT.to_owned(), format!("Output · {}", device(&self.graph.output, "none")), false),
            (INPUT.to_owned(), format!("Input · {}", device(&self.graph.input, "none")), false),
            (STEAM_ALL.to_owned(), "Steam games (all)".to_owned(), false),
        ];
        let mut seen = HashSet::new();
        for stream in &self.graph.streams {
            if seen.insert(&stream.identity) {
                let running = self.graph.streams.iter().any(|s| s.identity == stream.identity && s.running);
                options.push((stream.identity.clone(), stream.name.clone(), running));
            }
        }
        let current = self.assignment(index);
        if !current.is_empty() && !options.iter().any(|(id, ..)| id == current) {
            options.push((current.to_owned(), format!("{} (inactive)", self.config.applications[index]), false));
        }
        options
    }

    pub fn strip_title(&self, index: usize) -> String {
        match self.config.applications[index].as_str() {
            "" => "Unassigned".into(),
            STEAM_ALL => "Steam".into(),
            OUTPUT => "Output".into(),
            INPUT => "Input".into(),
            name => self.assigned_stream(index).map_or_else(|| name.to_owned(), |s| s.name.clone()),
        }
    }

    pub fn strip_detail(&self, index: usize) -> String {
        match self.config.applications[index].as_str() {
            "" => "No app".into(),
            STEAM_ALL => match self.assigned_streams(index).len() {
                0 => "No game running".into(),
                1 => "1 game".into(),
                count => format!("{count} games"),
            },
            OUTPUT => self.graph.output.as_ref().map_or_else(|| "No device".into(), |d| d.description.clone()),
            INPUT => self.graph.input.as_ref().map_or_else(|| "No device".into(), |d| d.description.clone()),
            _ => match self.assigned_stream(index) {
                Some(stream) => stream.detail(),
                None => "Not running".into(),
            },
        }
    }

    pub fn set_volume(&mut self, index: usize, volume: f32) {
        let volume = volume.clamp(0.0, 1.0);
        if self.config.fader_volumes[index] != volume {
            self.config.fader_volumes[index] = volume;
            self.apply(index);
            self.schedule_save();
        }
    }

    pub fn set_muted(&mut self, index: usize, muted: bool) {
        self.muted[index] = muted;
        self.mute_touched[index] = true;
        self.apply(index);
    }

    // MIDI ------------------------------------------------------------------------------------

    fn wants_midi(&self) -> bool {
        self.connected_port.is_empty() && !self.midi_connecting && self.config.midi_port != MIDI_DISCONNECTED
    }

    fn disconnect_midi(&mut self) {
        // Messages still queued from the old connection carry the old generation and are dropped.
        self.midi_generation = self.midi_generation.wrapping_add(1);
        self.midi_connecting = false;
        self.connected_port.clear();
        self.midi(midi::Command::Disconnect);
    }

    /// Picks a MIDI input: "" for Automatic, `MIDI_DISCONNECTED`, or a stable port name.
    pub fn choose_port(&mut self, choice: &str) {
        self.config.midi_port = choice.to_owned();
        self.disconnect_midi();
        self.connect_midi(Instant::now());
        self.schedule_save();
    }

    pub fn refresh_midi(&mut self) {
        self.disconnect_midi();
        self.midi_status = "Looking for MIDI devices…".into();
        self.last_ports = None;
        self.listing_ports = false;
        self.last_connect = Some(Instant::now());
        self.tick(Instant::now());
    }

    fn take_ports(&mut self, result: Result<Vec<String>, String>, now: Instant) {
        self.listing_ports = false;
        let mut ports = match result {
            Ok(ports) => ports,
            Err(error) => {
                self.midi_status = format!("MIDI unavailable · {error}");
                return;
            }
        };
        ports.sort_by_key(|port| port.to_lowercase());
        self.midi_ports = ports;
        if !self.connected_port.is_empty() && !self.midi_ports.contains(&self.connected_port) {
            self.disconnect_midi();
            self.midi_status = "MIDI device unplugged · waiting for it".into();
        }
        if self.wants_midi() {
            self.connect_midi(now);
        }
    }

    fn connect_midi(&mut self, now: Instant) {
        self.last_connect = Some(now);
        let wanted = self.config.midi_port.clone();
        if wanted == MIDI_DISCONNECTED {
            self.disconnect_midi();
            self.midi_status = "MIDI input off".into();
            return;
        }
        let port = if wanted.is_empty() {
            midi::preferred_port(&self.midi_ports)
        } else {
            self.midi_ports.iter().find(|port| midi::stable_port_name(port) == wanted)
        };
        let Some(port) = port.cloned() else {
            self.disconnect_midi();
            self.midi_status = if wanted.is_empty() {
                "No MIDI device · plug one in".into()
            } else {
                format!("{} unplugged · waiting for it", port_title(&wanted))
            };
            return;
        };
        if self.connected_port == port || self.midi_connecting {
            return;
        }
        self.disconnect_midi();
        self.midi_connecting = true;
        self.midi_status = format!("Connecting to {}…", port_title(&port));
        self.midi(midi::Command::Connect { generation: self.midi_generation, port });
    }

    fn take_connection(&mut self, generation: u64, port: String, result: Result<(), String>) {
        if generation != self.midi_generation {
            return;
        }
        self.midi_connecting = false;
        match result {
            Ok(()) => {
                self.midi_status = format!("Connected · {}", port_title(&port));
                self.connected_port = port;
            }
            Err(error) => {
                self.connected_port.clear();
                self.midi_status = format!("Couldn't open {} · {error} · retrying", port_title(&port));
            }
        }
    }

    pub fn midi_health(&self) -> Health {
        if !self.connected_port.is_empty() {
            Health::Good
        } else if self.config.midi_port == MIDI_DISCONNECTED {
            Health::Bad
        } else {
            Health::Waiting
        }
    }

    pub fn handle_midi(&mut self, data: &[u8], now: Instant) {
        let Some(message) = midi::parse(data) else {
            return;
        };
        if let Some(learn) = self.learning {
            // A fader learns a CC. A mute learns a CC or a note going down, not the release of a
            // key pressed before learning started.
            let (index, mute) = match learn {
                Learn::Fader(index) => (index, false),
                Learn::Mute(index) => (index, true),
            };
            let takes = match message {
                Message::Cc { .. } => true,
                Message::Note { on, .. } => mute && on,
            };
            if takes {
                self.config.bind(index, mute, message.control());
                self.learning = None;
                self.touched_at[index] = Some(now);
                self.schedule_save();
            }
            return;
        }
        for index in 0..FADER_COUNT {
            if let Message::Cc { value, .. } = message
                && self.config.fader_controls[index].is_some_and(|control| message.is(&control))
            {
                self.touched_at[index] = Some(now);
                self.set_volume(index, f32::from(value) / 127.0);
            }
            if self.config.mute_controls[index].is_some_and(|control| message.is(&control)) {
                self.touched_at[index] = Some(now);
                match self.config.mute_mode {
                    MuteMode::Latch => self.set_muted(index, message.pressed()),
                    MuteMode::Toggle if message.pressed() => self.set_muted(index, !self.muted[index]),
                    MuteMode::Toggle => {}
                }
            }
        }
    }
}

impl Drop for Mixer {
    fn drop(&mut self) {
        if self.engine.is_some() && self.save_at.is_some() {
            self.save_now();
        }
    }
}

/// A port as people know it: the device's name, without ALSA's client prefix and numbers.
pub(crate) fn port_title(port: &str) -> &str {
    let stable = midi::stable_port_name(port);
    stable.split_once(':').map_or(stable, |(_, port)| port)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::midi::Control;

    fn stream(node_id: u64, name: &str, steam: bool) -> AudioStream {
        AudioStream {
            node_id,
            name: name.into(),
            app_name: name.into(),
            app_id: String::new(),
            binary: String::new(),
            description: String::new(),
            identity: format!("stream-app:{}::", name.to_lowercase()),
            is_steam_game: steam,
            running: true,
        }
    }

    fn mixer_with(streams: Vec<AudioStream>) -> Mixer {
        let mut mixer = Mixer::new(Config::default());
        mixer.take_graph(Ok(Graph { streams, ..Graph::default() }));
        mixer
    }

    #[test]
    fn faders_follow_their_learned_cc_on_any_channel() {
        let mut mixer = mixer_with(vec![]);
        let now = Instant::now();
        mixer.handle_midi(&[0xb3, 2, 127], now);
        assert_eq!(mixer.config.fader_volumes[1], 1.0);
        mixer.learning = Some(Learn::Fader(1));
        mixer.handle_midi(&[0xb0, 20, 0], now);
        assert_eq!(mixer.learning, None);
        assert_eq!(mixer.config.fader_controls[1], Some(Control { channel: Some(0), ..Control::cc(20) }));
        mixer.handle_midi(&[0xb1, 20, 0], now);
        assert_eq!(mixer.config.fader_volumes[1], 1.0, "learned on channel 1 only, so channel 2 is ignored");
        mixer.handle_midi(&[0xb0, 20, 0], now);
        assert_eq!(mixer.config.fader_volumes[1], 0.0);
    }

    #[test]
    fn mute_buttons_latch_or_toggle() {
        let mut mixer = mixer_with(vec![]);
        let now = Instant::now();
        mixer.handle_midi(&[0x90, 37, 100], now);
        assert!(mixer.muted[0]);
        mixer.handle_midi(&[0x90, 37, 0], now);
        assert!(!mixer.muted[0], "a latching switch follows the note off");
        mixer.config.mute_mode = MuteMode::Toggle;
        mixer.handle_midi(&[0x90, 37, 100], now);
        mixer.handle_midi(&[0x80, 37, 0], now);
        assert!(mixer.muted[0], "a momentary button flips on its press only");
        mixer.handle_midi(&[0x90, 37, 100], now);
        assert!(!mixer.muted[0]);
    }

    #[test]
    fn learning_a_mute_ignores_a_stray_release() {
        let mut mixer = mixer_with(vec![]);
        mixer.learning = Some(Learn::Mute(4));
        mixer.handle_midi(&[0x80, 60, 0], Instant::now());
        assert_eq!(mixer.learning, Some(Learn::Mute(4)));
        mixer.handle_midi(&[0x99, 60, 90], Instant::now());
        assert_eq!(mixer.config.mute_controls[4], Some(Control { channel: Some(9), ..Control::note(60) }));
    }

    #[test]
    fn steam_strips_drive_every_game_and_apps_drive_every_node() {
        let mut mixer = mixer_with(vec![stream(1, "Game", true), stream(2, "Firefox", false), stream(3, "Firefox", false)]);
        mixer.assign(0, STEAM_ALL);
        let firefox = mixer.graph.streams[1].identity.clone();
        mixer.assign(1, &firefox);
        assert_eq!(mixer.targets(0), ["1"]);
        assert_eq!(mixer.targets(1), ["2", "3"]);
        assert_eq!(mixer.strip_title(1), "Firefox");
        assert!(mixer.is_live(1));
    }

    #[test]
    fn a_vanished_app_stays_assigned_and_listed() {
        let mut mixer = mixer_with(vec![stream(2, "Firefox", false)]);
        let firefox = mixer.graph.streams[0].identity.clone();
        mixer.assign(0, &firefox);
        mixer.take_graph(Ok(Graph::default()));
        assert_eq!(mixer.assignment(0), firefox);
        assert!(!mixer.is_present(0));
        let options = mixer.assignment_options(0);
        assert_eq!(options.last().unwrap().1, "Firefox (inactive)");
    }

    #[test]
    fn port_titles_drop_the_client_and_numbers() {
        assert_eq!(port_title("nanoKONTROL2:nanoKONTROL2 _ CTRL 24:0"), "nanoKONTROL2 _ CTRL");
        assert_eq!(port_title("Plain"), "Plain");
    }
}
