use crate::stream::{self, AudioStream};
use midir::MidiInput;
use std::{
    collections::HashMap,
    process::Command,
    sync::mpsc::{self, Receiver, Sender},
    thread,
};

pub(crate) enum EngineCommand {
    DiscoverStreams,
    DiscoverMidiPorts,
    SetVolume(u64, f32),
    SetMute(u64, bool),
}

pub(crate) enum EngineEvent {
    Streams(Result<Vec<AudioStream>, String>),
    MidiPorts(Result<Vec<String>, String>),
    CommandError(String),
}

pub(crate) struct Engine {
    pub sender: Sender<EngineCommand>,
    pub events: Receiver<EngineEvent>,
}

impl Engine {
    pub fn start() -> Self {
        let (sender, commands) = mpsc::channel();
        let (events_tx, events) = mpsc::channel();
        thread::Builder::new()
            .name("mixer-engine".into())
            .spawn(move || {
                while let Ok(first_command) = commands.recv() {
                    let mut batch = vec![first_command];
                    batch.extend(commands.try_iter());
                    let mut volumes = HashMap::new();
                    let mut mutes = HashMap::new();
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
                            EngineCommand::SetVolume(node_id, volume) => {
                                volumes.insert(node_id, volume);
                            }
                            EngineCommand::SetMute(node_id, muted) => {
                                mutes.insert(node_id, muted);
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
                }
            })
            .expect("could not start audio engine worker");
        Self { sender, events }
    }
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
