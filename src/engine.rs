use crate::stream::{self, AudioStream};
use midir::{Ignore, MidiInput, MidiInputConnection};
use std::{
    collections::HashMap,
    process::Command,
    sync::mpsc::{self, Receiver, Sender},
    thread,
};

pub(crate) enum EngineCommand {
    DiscoverStreams,
    DiscoverMidiPorts,
    ConnectMidi { generation: u64, port_name: String },
    DisconnectMidi,
    SetVolume(u64, f32),
    SetMute(u64, bool),
    Shutdown,
}

pub(crate) enum EngineEvent {
    Streams(Result<Vec<AudioStream>, String>),
    MidiPorts(Result<Vec<String>, String>),
    MidiConnection {
        generation: u64,
        port_name: String,
        result: Result<(), String>,
    },
    CommandError(String),
}

enum MidiCommand {
    Connect { generation: u64, port_name: String },
    Disconnect,
}

pub(crate) struct Engine {
    pub sender: Sender<EngineCommand>,
    pub events: Receiver<EngineEvent>,
    pub midi_messages: Receiver<(u64, Vec<u8>)>,
}

impl Engine {
    pub fn start() -> Self {
        let (sender, commands) = mpsc::channel();
        let (events_tx, events) = mpsc::channel();
        let (midi_messages_tx, midi_messages) = mpsc::sync_channel(256);
        let (midi_commands_tx, midi_commands) = mpsc::channel();
        let midi_events = events_tx.clone();
        thread::Builder::new()
            .name("mixer-midi".into())
            .spawn(move || run_midi_worker(midi_commands, midi_events, midi_messages_tx))
            .expect("could not start MIDI worker");
        thread::Builder::new()
            .name("mixer-engine".into())
            .spawn(move || {
                while let Ok(first_command) = commands.recv() {
                    let mut batch = vec![first_command];
                    batch.extend(commands.try_iter());
                    let mut volumes = HashMap::new();
                    let mut mutes = HashMap::new();
                    let mut shutdown = false;
                    for command in batch {
                        match command {
                            EngineCommand::DiscoverStreams => {
                                let event_sender = events_tx.clone();
                                thread::spawn(move || {
                                    let result =
                                        stream::discover().map_err(|error| error.to_string());
                                    let _ = event_sender.send(EngineEvent::Streams(result));
                                });
                            }
                            EngineCommand::DiscoverMidiPorts => {
                                let event_sender = events_tx.clone();
                                thread::spawn(move || {
                                    let result = MidiInput::new("Viper Mixer")
                                        .map(|input| {
                                            input
                                                .ports()
                                                .iter()
                                                .filter_map(|port| input.port_name(port).ok())
                                                .collect()
                                        })
                                        .map_err(|error| error.to_string());
                                    let _ = event_sender.send(EngineEvent::MidiPorts(result));
                                });
                            }
                            EngineCommand::ConnectMidi {
                                generation,
                                port_name,
                            } => {
                                let _ = midi_commands_tx.send(MidiCommand::Connect {
                                    generation,
                                    port_name,
                                });
                            }
                            EngineCommand::DisconnectMidi => {
                                let _ = midi_commands_tx.send(MidiCommand::Disconnect);
                            }
                            EngineCommand::SetVolume(node_id, volume) => {
                                volumes.insert(node_id, volume);
                            }
                            EngineCommand::SetMute(node_id, muted) => {
                                mutes.insert(node_id, muted);
                            }
                            EngineCommand::Shutdown => {
                                shutdown = true;
                                break;
                            }
                        }
                    }
                    for (node_id, volume) in volumes {
                        if let Err(error) = run_wpctl(&[
                            "set-volume",
                            &node_id.to_string(),
                            &format!("{volume:.3}"),
                        ]) {
                            let _ = events_tx.send(EngineEvent::CommandError(error));
                        }
                    }
                    for (node_id, muted) in mutes {
                        if let Err(error) = run_wpctl(&[
                            "set-mute",
                            &node_id.to_string(),
                            if muted { "1" } else { "0" },
                        ]) {
                            let _ = events_tx.send(EngineEvent::CommandError(error));
                        }
                    }
                    if shutdown {
                        // Pending volume and mute changes above are flushed before exiting.
                        let _ = midi_commands_tx.send(MidiCommand::Disconnect);
                        return;
                    }
                }
            })
            .expect("could not start audio engine worker");
        Self {
            sender,
            events,
            midi_messages,
        }
    }

    pub fn shutdown(&self) {
        let _ = self.sender.send(EngineCommand::Shutdown);
    }
}

fn run_midi_worker(
    commands: Receiver<MidiCommand>,
    events: Sender<EngineEvent>,
    midi_messages: mpsc::SyncSender<(u64, Vec<u8>)>,
) {
    let mut connection: Option<MidiInputConnection<()>> = None;
    while let Ok(command) = commands.recv() {
        match command {
            MidiCommand::Disconnect => connection = None,
            MidiCommand::Connect {
                generation,
                port_name,
            } => {
                // Opening and dropping MIDI connections can block in backend code. Keep both off
                // the UI and PipeWire command threads.
                connection = None;
                let result = connect_midi(&port_name, generation, midi_messages.clone());
                match result {
                    Ok(opened) => {
                        connection = Some(opened);
                        let _ = events.send(EngineEvent::MidiConnection {
                            generation,
                            port_name,
                            result: Ok(()),
                        });
                    }
                    Err(error) => {
                        let _ = events.send(EngineEvent::MidiConnection {
                            generation,
                            port_name,
                            result: Err(error),
                        });
                    }
                }
            }
        }
    }
    drop(connection);
}

fn connect_midi(
    port_name: &str,
    generation: u64,
    midi_messages: mpsc::SyncSender<(u64, Vec<u8>)>,
) -> Result<MidiInputConnection<()>, String> {
    let mut input = MidiInput::new("Viper Mixer").map_err(|error| error.to_string())?;
    input.ignore(Ignore::None);
    let port = input
        .ports()
        .into_iter()
        .find(|port| input.port_name(port).ok().as_deref() == Some(port_name))
        .ok_or_else(|| "MIDI device changed · retrying".to_owned())?;
    input
        .connect(
            &port,
            "viper-midi-mixer-input",
            move |_timestamp, message, _| {
                let _ = midi_messages.try_send((generation, message.to_vec()));
            },
            (),
        )
        .map_err(|error| error.to_string())
}

fn run_wpctl(arguments: &[&str]) -> Result<(), String> {
    let output = Command::new("wpctl")
        .args(arguments)
        .output()
        .map_err(|error| format!("Could not run wpctl: {error}"))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "wpctl {} failed: {}",
            arguments.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}
