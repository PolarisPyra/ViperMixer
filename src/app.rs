use crate::{
    config::{Config, FADER_COUNT, MIDI_DISCONNECTED, STEAM_ALL},
    engine::{Engine, EngineCommand, EngineEvent},
    stream::AudioStream,
};
use eframe::egui;
use std::{
    collections::HashSet,
    sync::mpsc::{self, Receiver, Sender},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

const LOGO: &[u8] = include_bytes!("../assets/logo.png");
const TRAY_SHOW: &str = "show";
const TRAY_QUIT: &str = "quit";

enum TrayEvent {
    Show,
    Quit,
}

struct MixerApp {
    config: Config,
    streams: Vec<AudioStream>,
    midi_ports: Vec<String>,
    connected_port: String,
    midi_generation: u64,
    midi_connecting: bool,
    engine: Engine,
    tray_receiver: Receiver<TrayEvent>,
    tray_available: bool,
    window_hides: bool,
    quit_requested: bool,
    shutdown_requested: bool,
    closed_to_tray: bool,
    status: String,
    stream_status: String,
    muted: [bool; FADER_COUNT],
    mute_touched: [bool; FADER_COUNT],
    learning: Option<usize>,
    last_stream_request: Instant,
    last_ports_request: Instant,
    last_connect_attempt: Instant,
    pending_streams: bool,
    pending_ports: bool,
    save_at: Option<Instant>,
    last_applied: Vec<(u64, u32, Option<bool>)>,
    error: Option<String>,
}

impl MixerApp {
    fn new(tray_receiver: Receiver<TrayEvent>, tray_available: bool) -> Self {
        let engine = Engine::start();
        let now = Instant::now();
        let mut app = Self {
            config: Config::load(),
            streams: Vec::new(),
            midi_ports: Vec::new(),
            connected_port: String::new(),
            midi_generation: 0,
            midi_connecting: false,
            engine,
            tray_receiver,
            tray_available,
            window_hides: window_can_hide_here(),
            quit_requested: false,
            closed_to_tray: false,
            shutdown_requested: false,
            status: "Looking for MIDI devices…".into(),
            stream_status: "Connecting to PipeWire…".into(),
            muted: [false; FADER_COUNT],
            mute_touched: [false; FADER_COUNT],
            learning: None,
            last_stream_request: now - Duration::from_secs(5),
            last_ports_request: now - Duration::from_secs(5),
            last_connect_attempt: now - Duration::from_secs(5),
            pending_streams: false,
            pending_ports: false,
            save_at: None,
            last_applied: Vec::new(),
            error: None,
        };
        // Don't block on startup - defer connection attempts to the tick loop
        app.request_streams();
        app.request_ports();
        app
    }

    fn schedule_save(&mut self) {
        self.save_at = Some(Instant::now() + Duration::from_millis(350));
    }

    fn request_streams(&mut self) {
        if self.pending_streams {
            return;
        }
        self.pending_streams = self
            .engine
            .sender
            .send(EngineCommand::DiscoverStreams)
            .is_ok();
        self.last_stream_request = Instant::now();
    }

    fn request_ports(&mut self) {
        if self.pending_ports {
            return;
        }
        self.pending_ports = self
            .engine
            .sender
            .send(EngineCommand::DiscoverMidiPorts)
            .is_ok();
        self.last_ports_request = Instant::now();
    }

    fn refresh_midi(&mut self) {
        self.disconnect_midi();
        self.connected_port.clear();
        self.midi_connecting = false;
        self.status = "Refreshing MIDI devices…".into();
        self.last_connect_attempt = Instant::now();
        self.request_ports();
    }

    fn update_ports(&mut self, mut ports: Vec<String>) {
        ports.sort_by_key(|port| port.to_lowercase());
        self.midi_ports = ports;
        if !self.connected_port.is_empty() && !self.midi_ports.contains(&self.connected_port) {
            self.disconnect_midi();
            self.connected_port.clear();
            self.midi_connecting = false;
            self.status = "MIDI device disconnected · reconnecting".into();
        }
        if self.connected_port.is_empty()
            && !self.midi_connecting
            && self.config.midi_port != MIDI_DISCONNECTED
        {
            self.connect_midi();
        }
    }

    fn disconnect_midi(&mut self) {
        self.midi_generation = self.midi_generation.wrapping_add(1);
        let _ = self.engine.sender.send(EngineCommand::DisconnectMidi);
    }

    fn connect_midi(&mut self) {
        self.last_connect_attempt = Instant::now();
        if self.config.midi_port == MIDI_DISCONNECTED {
            self.disconnect_midi();
            self.midi_connecting = false;
            self.connected_port.clear();
            self.status = "MIDI input disconnected".into();
            return;
        }
        if !self.config.midi_port.is_empty() && !self.midi_ports.contains(&self.config.midi_port) {
            self.disconnect_midi();
            self.connected_port.clear();
            self.midi_connecting = false;
            self.status = format!("{} unavailable · reconnecting", self.config.midi_port);
            return;
        }
        let selected = if self.config.midi_port.is_empty() {
            preferred_midi_port(&self.midi_ports)
        } else {
            self.midi_ports
                .iter()
                .find(|port| **port == self.config.midi_port)
        };
        let Some(port_name) = selected.cloned() else {
            self.disconnect_midi();
            self.midi_connecting = false;
            self.connected_port.clear();
            self.status = "No MIDI input found · reconnecting automatically".into();
            return;
        };
        if self.connected_port == port_name && !self.midi_connecting {
            return;
        }
        self.midi_generation = self.midi_generation.wrapping_add(1);
        self.midi_connecting = true;
        self.status = format!("Connecting to {port_name}…");
        if self
            .engine
            .sender
            .send(EngineCommand::ConnectMidi {
                generation: self.midi_generation,
                port_name,
            })
            .is_err()
        {
            self.midi_connecting = false;
            self.status = "MIDI service unavailable".into();
        }
    }

    fn assigned_stream(&self, index: usize) -> Option<&AudioStream> {
        let identity = &self.config.stream_keys[index];
        if !identity.is_empty() {
            if let Some(stream) = self
                .streams
                .iter()
                .find(|stream| stream.identity() == *identity)
            {
                return Some(stream);
            }
        }
        let name = &self.config.applications[index];
        if name.is_empty() || name == STEAM_ALL {
            return None;
        }
        self.streams.iter().find(|stream| stream.name == *name)
    }

    fn assigned_ids(&self, index: usize) -> Vec<u64> {
        let selected = &self.config.applications[index];
        if selected == STEAM_ALL {
            self.streams
                .iter()
                .filter(|stream| stream.is_steam_game())
                .map(|stream| stream.node_id)
                .collect()
        } else {
            self.assigned_stream(index)
                .map(|stream| vec![stream.node_id])
                .unwrap_or_default()
        }
    }

    fn apply(&mut self, index: usize) {
        let ids = self.assigned_ids(index);
        let volume = self.config.fader_volumes[index];
        for id in ids {
            let volume_key = (volume * 10000.0).round() as u32;
            let previous = self
                .last_applied
                .iter()
                .find(|(last_id, _, _)| *last_id == id)
                .copied();
            if previous.is_none_or(|(_, previous_volume, _)| previous_volume != volume_key) {
                let _ = self
                    .engine
                    .sender
                    .send(EngineCommand::SetVolume(id, volume));
            }
            if self.mute_touched[index]
                && previous
                    .is_none_or(|(_, _, previous_mute)| previous_mute != Some(self.muted[index]))
            {
                let _ = self
                    .engine
                    .sender
                    .send(EngineCommand::SetMute(id, self.muted[index]));
            }
            self.last_applied.retain(|(last_id, _, _)| *last_id != id);
            self.last_applied.push((
                id,
                volume_key,
                self.mute_touched[index].then_some(self.muted[index]),
            ));
        }
    }

    fn sync_all(&mut self) {
        for index in 0..FADER_COUNT {
            self.apply(index);
        }
    }

    fn handle_midi(&mut self, data: &[u8]) {
        if data.len() < 3 {
            return;
        }
        let command = data[0] & 0xf0;
        if let Some(index) = self.learning {
            if command == 0x90 || command == 0x80 {
                self.config.mute_notes[index] = Some(data[1]);
                self.learning = None;
                self.schedule_save();
            }
            return;
        }
        if command == 0xb0 && (1..=FADER_COUNT as u8).contains(&data[1]) {
            let index = usize::from(data[1] - 1);
            self.config.fader_volumes[index] = f32::from(data[2]) / 127.0;
            self.apply(index);
            self.schedule_save();
        } else if command == 0x90 || command == 0x80 {
            let muted = command == 0x90 && data[2] > 0;
            for index in 0..FADER_COUNT {
                if self.config.mute_notes[index] == Some(data[1]) {
                    self.muted[index] = muted;
                    self.mute_touched[index] = true;
                    self.apply(index);
                }
            }
        }
    }

    fn process_events(&mut self) {
        while let Ok(event) = self.engine.events.try_recv() {
            match event {
                EngineEvent::Streams(Ok(streams)) => {
                    self.pending_streams = false;
                    let active_ids = streams
                        .iter()
                        .map(|stream| stream.node_id)
                        .collect::<HashSet<_>>();
                    let reused_ids = streams
                        .iter()
                        .filter(|next| {
                            self.streams
                                .iter()
                                .find(|previous| previous.node_id == next.node_id)
                                .is_some_and(|previous| previous.identity() != next.identity())
                        })
                        .map(|stream| stream.node_id)
                        .collect::<HashSet<_>>();
                    self.last_applied.retain(|(node_id, _, _)| {
                        active_ids.contains(node_id) && !reused_ids.contains(node_id)
                    });
                    self.streams = streams;
                    let mut assignments_changed = false;
                    for index in 0..FADER_COUNT {
                        if let Some(stream) = self.assigned_stream(index) {
                            let identity = stream.identity();
                            if self.config.stream_keys[index] != identity {
                                self.config.stream_keys[index] = identity;
                                assignments_changed = true;
                            }
                        }
                    }
                    if assignments_changed {
                        self.schedule_save();
                    }
                    self.stream_status =
                        format!("{} audio application(s) detected", self.streams.len());
                    self.sync_all();
                }
                EngineEvent::Streams(Err(error)) => {
                    self.pending_streams = false;
                    self.stream_status = format!("PipeWire unavailable · {error}");
                }
                EngineEvent::MidiPorts(Ok(ports)) => {
                    self.pending_ports = false;
                    if ports != self.midi_ports
                        || (self.connected_port.is_empty()
                            && !self.midi_connecting
                            && self.config.midi_port != MIDI_DISCONNECTED)
                    {
                        self.update_ports(ports);
                    }
                }
                EngineEvent::MidiPorts(Err(error)) => {
                    self.pending_ports = false;
                    self.status = format!("MIDI unavailable · {error}");
                }
                EngineEvent::MidiConnection {
                    generation,
                    port_name,
                    result,
                } => {
                    if generation == self.midi_generation {
                        self.midi_connecting = false;
                        match result {
                            Ok(()) => {
                                self.connected_port = port_name.clone();
                                self.status = format!("Connected · {port_name}");
                            }
                            Err(error) => {
                                self.connected_port.clear();
                                self.status = format!(
                                    "Could not connect to {port_name} · {error} · retrying"
                                );
                            }
                        }
                    }
                }
                EngineEvent::CommandError(error) => {
                    self.last_applied.clear();
                    self.error = Some(error);
                }
            }
        }
        while let Ok((generation, data)) = self.engine.midi_messages.try_recv() {
            if generation == self.midi_generation {
                self.handle_midi(&data);
            }
        }
        if self.save_at.is_some_and(|time| Instant::now() >= time) {
            if let Err(error) = self.config.save() {
                self.error = Some(format!("Could not save settings: {error}"));
            }
            self.save_at = None;
        }
    }

    fn show_fader(&mut self, ui: &mut egui::Ui, index: usize) {
        let id = egui::Id::new(("fader-card", index));
        let saved_name = self.config.applications[index].clone();
        let saved_identity = self.config.stream_keys[index].clone();
        let current = if saved_name == STEAM_ALL {
            STEAM_ALL.to_owned()
        } else if !saved_identity.is_empty() {
            saved_identity.clone()
        } else {
            saved_name.clone()
        };
        let assigned_stream = if current == STEAM_ALL {
            None
        } else {
            self.assigned_stream(index)
        };
        let assigned_name = assigned_stream.map(|stream| stream.name.clone());
        let has_assigned_stream = assigned_name.is_some();
        let selected_label = if current.is_empty() {
            "No Application Selected".to_owned()
        } else if current == STEAM_ALL {
            "Steam Games (All)".to_owned()
        } else if let Some(name) = assigned_name.as_ref() {
            name.clone()
        } else {
            format!("{} (Inactive)", saved_name)
        };
        ui.horizontal(|ui| {
            ui.set_min_height(36.0);
            ui.add_sized(
                [70.0, 30.0],
                egui::Label::new(egui::RichText::new(format!("Fader {}", index + 1)).strong()),
            );
            let mut selected = current.clone();
            let control_width = 75.0 + 125.0 + 90.0 + 46.0;
            let combo_width = (ui.available_width() - control_width).max(320.0);
            dark_combo(ui, |ui| {
                egui::ComboBox::from_id_salt(id)
                    .width(combo_width)
                    .selected_text(selected_label)
                    .show_ui(ui, |ui| {
                        ui.selectable_value(
                            &mut selected,
                            String::new(),
                            "No Application Selected",
                        );
                        ui.selectable_value(
                            &mut selected,
                            STEAM_ALL.to_owned(),
                            "Steam Games (All)",
                        );
                        for stream in &self.streams {
                            let identity = stream.identity();
                            ui.selectable_value(&mut selected, identity, stream.name.clone())
                                .on_hover_text(stream.detail());
                        }
                        if !saved_name.is_empty() && saved_name != STEAM_ALL && !has_assigned_stream
                        {
                            ui.selectable_value(
                                &mut selected,
                                current.clone(),
                                format!("{saved_name} (Inactive)"),
                            );
                        }
                    });
            });
            ui.add_space(4.0);
            if selected != current {
                if selected.is_empty() {
                    self.config.applications[index].clear();
                    self.config.stream_keys[index].clear();
                } else if selected == STEAM_ALL {
                    self.config.applications[index] = STEAM_ALL.to_owned();
                    self.config.stream_keys[index].clear();
                } else if let Some(stream) = self
                    .streams
                    .iter()
                    .find(|stream| stream.identity() == selected)
                {
                    self.config.applications[index] = stream.name.clone();
                    self.config.stream_keys[index] = selected;
                }
                self.last_applied.clear();
                self.apply(index);
                self.schedule_save();
            }
            let mut muted = self.muted[index];
            if ui.checkbox(&mut muted, "Mute").changed() {
                self.muted[index] = muted;
                self.mute_touched[index] = true;
                self.apply(index);
            }
            let note = self.config.mute_notes[index].map_or_else(
                || "Bind Mute Note".to_owned(),
                |note| format!("Mute Note: {note}"),
            );
            let label = if self.learning == Some(index) {
                "Press mute key…"
            } else {
                &note
            };
            if ui
                .add_sized([125.0, 28.0], egui::Button::new(label))
                .clicked()
            {
                self.learning = if self.learning == Some(index) {
                    None
                } else {
                    Some(index)
                };
            }
            ui.add_sized(
                [90.0, 24.0],
                egui::Label::new(
                    egui::RichText::new(format!(
                        "Vol: {}%",
                        (self.config.fader_volumes[index] * 100.0).round() as u32
                    ))
                    .color(ui.visuals().weak_text_color()),
                ),
            );
        });
    }
}

impl MixerApp {
    fn tick(&mut self) {
        self.process_events();
        if Instant::now().duration_since(self.last_stream_request) > Duration::from_secs(2) {
            self.request_streams();
        }
        if Instant::now().duration_since(self.last_ports_request) > Duration::from_secs(3) {
            self.request_ports();
        }
        if self.connected_port.is_empty()
            && !self.midi_connecting
            && self.config.midi_port != MIDI_DISCONNECTED
            && Instant::now().duration_since(self.last_connect_attempt) >= Duration::from_secs(2)
        {
            self.connect_midi();
        }
    }

    fn logic(&mut self, ctx: &egui::Context) {
        self.tick();
        while let Ok(event) = self.tray_receiver.try_recv() {
            match event {
                TrayEvent::Show => {
                    if self.window_hides {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                        ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
                        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                    }
                }
                TrayEvent::Quit => {
                    self.shutdown_requested = true;
                    self.quit_requested = true;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            }
        }
        if !self.quit_requested && ctx.input(|input| input.viewport().close_requested()) {
            if self.tray_available {
                if self.window_hides {
                    ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                    ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
                } else {
                    // Wayland cannot reliably remap a hidden window. Let eframe close it and
                    // recreate a fresh native window when the user asks to show it from the tray.
                    self.closed_to_tray = true;
                }
            } else {
                // No tray available - actually quit
                self.shutdown_requested = true;
                self.quit_requested = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }
        if self.shutdown_requested {
            self.quit_requested = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        ctx.request_repaint_after(Duration::from_millis(120));
    }

    fn ui(&mut self, ui: &mut egui::Ui) {
        style(ui.ctx());
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(egui::Color32::from_rgb(32, 34, 36)).inner_margin(20.0))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("MIDI Device").strong());
                    let selected = if self.config.midi_port == MIDI_DISCONNECTED { "Disconnected" } else if self.config.midi_port.is_empty() { "Automatic" } else { &self.config.midi_port };
                    let mut desired_port = self.config.midi_port.clone();
                    dark_combo(ui, |ui| {
                        egui::ComboBox::from_id_salt("midi-device").width(360.0).selected_text(selected).show_ui(ui, |ui| {
                            ui.selectable_value(&mut desired_port, String::new(), "Automatic");
                            ui.selectable_value(&mut desired_port, MIDI_DISCONNECTED.to_owned(), "Disconnected");
                            for port in &self.midi_ports { ui.selectable_value(&mut desired_port, port.clone(), port); }
                        });
                    });
                    if desired_port != self.config.midi_port {
                        self.disconnect_midi();
                        self.config.midi_port = desired_port;
                        self.connected_port.clear();
                        self.midi_connecting = false;
                        self.connect_midi();
                        self.schedule_save();
                    }
                    if ui.button("Refresh").clicked() { self.refresh_midi(); }
                    let midi_status = if !self.connected_port.is_empty() {
                        format!("Connected: {}", self.connected_port)
                    } else {
                        self.status.clone()
                    };
                    ui.colored_label(if !self.connected_port.is_empty() { egui::Color32::from_rgb(55, 190, 112) } else { egui::Color32::from_rgb(213, 159, 76) }, midi_status);
                });
            ui.add_space(14.0);
            ui.heading("Assign Applications to Faders");
            ui.add_space(4.0);
            ui.label(egui::RichText::new("Choose the PipeWire application controlled by each MIDI fader. Choose Steam Games (All) to control every Steam game audio stream. Faders 1–5 map to MIDI CC 1–5. Latched hardware mute buttons sync mute states directly based on switch state.").size(12.0).color(ui.visuals().weak_text_color()));
            ui.add_space(12.0);
            egui::ScrollArea::vertical().auto_shrink([false, true]).show(ui, |ui| {
                for index in 0..FADER_COUNT {
                    self.show_fader(ui, index);
                    ui.add_space(4.0);
                }
            });
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(&self.stream_status).size(12.0).color(ui.visuals().weak_text_color()));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Save").clicked() {
                        match self.config.save() {
                            Ok(()) => self.stream_status = "Configuration saved".into(),
                            Err(error) => self.error = Some(format!("Could not save settings: {error}")),
                        }
                    }
                    if ui.button("Refresh Applications").clicked() { self.request_streams(); }
                });
            });
            if let Some(error) = &self.error {
                ui.add_space(8.0);
                ui.colored_label(egui::Color32::from_rgb(255, 130, 145), error);
            }
        });
    }
}

struct MixerWindow(Arc<Mutex<MixerApp>>);

impl eframe::App for MixerWindow {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if let Ok(mut app) = self.0.lock() {
            app.logic(ctx);
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        if let Ok(mut app) = self.0.lock() {
            app.ui(ui);
        }
    }
}

fn preferred_midi_port(ports: &[String]) -> Option<&String> {
    ports
        .iter()
        .find(|port| {
            let normalized = port.to_ascii_lowercase();
            !["midi through", "virtual", "dummy", "loopback"]
                .iter()
                .any(|marker| normalized.contains(marker))
        })
        .or_else(|| ports.first())
}

fn style(ctx: &egui::Context) {
    let mut visuals = egui::Visuals::dark();
    visuals.window_fill = egui::Color32::from_rgb(32, 34, 36);
    visuals.panel_fill = egui::Color32::from_rgb(32, 34, 36);
    visuals.faint_bg_color = egui::Color32::from_rgb(42, 44, 46);
    visuals.extreme_bg_color = egui::Color32::from_rgb(22, 24, 25);
    visuals.selection.bg_fill = egui::Color32::from_rgb(78, 112, 170);
    visuals.selection.stroke = egui::Stroke::new(1.0_f32, egui::Color32::WHITE);
    visuals.widgets.active.bg_fill = egui::Color32::from_rgb(78, 112, 170);
    visuals.widgets.hovered.bg_fill = egui::Color32::from_rgb(67, 70, 73);
    visuals.widgets.inactive.bg_fill = egui::Color32::from_rgb(52, 55, 57);
    visuals.widgets.noninteractive.bg_stroke =
        egui::Stroke::new(1.0_f32, egui::Color32::from_rgb(62, 65, 67));
    ctx.set_visuals(visuals);
}

fn dark_combo<R>(ui: &mut egui::Ui, add_contents: impl FnOnce(&mut egui::Ui) -> R) -> R {
    ui.scope(|ui| {
        let visuals = ui.visuals_mut();
        visuals.widgets.inactive.bg_fill = egui::Color32::from_rgb(21, 23, 25);
        visuals.widgets.inactive.weak_bg_fill = egui::Color32::from_rgb(21, 23, 25);
        visuals.widgets.hovered.bg_fill = egui::Color32::from_rgb(34, 37, 39);
        visuals.widgets.hovered.weak_bg_fill = egui::Color32::from_rgb(34, 37, 39);
        visuals.widgets.active.bg_fill = egui::Color32::from_rgb(42, 45, 47);
        visuals.widgets.active.weak_bg_fill = egui::Color32::from_rgb(42, 45, 47);
        add_contents(ui)
    })
    .inner
}

pub(crate) fn run() -> eframe::Result<()> {
    let decoded_logo = image::load_from_memory(LOGO).expect("logo.png must be a valid image");
    let icon_image = decoded_logo.thumbnail(256, 256).into_rgba8();
    let (width, height) = icon_image.dimensions();
    let icon = egui::IconData {
        rgba: icon_image.into_raw(),
        width,
        height,
    };
    let (tray_sender, tray_receiver) = mpsc::channel();
    let tray_icon = create_tray(tray_sender);
    let tray_available = tray_icon.is_ok();
    let tray_icon = match tray_icon {
        Ok(icon) => Some(icon),
        Err(error) => {
            eprintln!("System tray unavailable: {error}");
            None
        }
    };
    let app = Arc::new(Mutex::new(MixerApp::new(tray_receiver, tray_available)));
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Viper Mixer")
            .with_app_id("viper-midi-mixer")
            .with_inner_size([986.0, 660.0])
            .with_min_inner_size([980.0, 620.0])
            .with_icon(icon),
        renderer: eframe::Renderer::Wgpu,
        wgpu_options: wgpu_options(),
        ..Default::default()
    };
    let _tray_icon = tray_icon;
    if !tray_available {
        return eframe::run_native(
            "Viper Mixer",
            options,
            Box::new(move |cc| {
                let mut style = (*cc.egui_ctx.style_of(egui::Theme::Dark)).clone();
                style.spacing.item_spacing = egui::vec2(10.0, 8.0);
                cc.egui_ctx.set_style_of(egui::Theme::Dark, style);
                Ok(Box::new(MixerWindow(app.clone())))
            }),
        );
    }

    loop {
        // Keep all service discovery and tray handling alive while the native window is closed.
        // WGPU and the windowing backend are initialized only when the user asks to show it.
        loop {
            // Drop the mutex guard before handling Timeout: tick() needs the same lock.
            let event = {
                let app = app.lock().unwrap();
                if app.shutdown_requested {
                    return Ok(());
                }
                app.tray_receiver.recv_timeout(Duration::from_millis(120))
            };
            match event {
                Ok(TrayEvent::Quit) | Err(mpsc::RecvTimeoutError::Disconnected) => return Ok(()),
                Ok(TrayEvent::Show) => break,
                Err(mpsc::RecvTimeoutError::Timeout) => app.lock().unwrap().tick(),
            }
        }

        eframe::run_native(
            "Viper Mixer",
            options.clone(),
            Box::new({
                let app = app.clone();
                move |cc| {
                    let mut style = (*cc.egui_ctx.style_of(egui::Theme::Dark)).clone();
                    style.spacing.item_spacing = egui::vec2(10.0, 8.0);
                    cc.egui_ctx.set_style_of(egui::Theme::Dark, style);
                    Ok(Box::new(MixerWindow(app.clone())))
                }
            }),
        )?;

        let mut app = app.lock().unwrap();
        if app.quit_requested || app.shutdown_requested || !app.closed_to_tray {
            return Ok(());
        }
        app.closed_to_tray = false;
    }
}

#[cfg(target_os = "linux")]
fn window_can_hide_here() -> bool {
    !["WAYLAND_DISPLAY", "WAYLAND_SOCKET"]
        .iter()
        .any(|name| std::env::var_os(name).is_some_and(|value| !value.is_empty()))
}

#[cfg(not(target_os = "linux"))]
fn window_can_hide_here() -> bool {
    true
}

#[cfg(not(any(windows, target_os = "macos")))]
fn wgpu_options() -> eframe::WgpuConfiguration {
    use eframe::wgpu::{DeviceType, PowerPreference};
    let mut config = eframe::WgpuConfiguration::default();
    if let eframe::egui_wgpu::WgpuSetup::CreateNew(setup) = &mut config.wgpu_setup {
        if let Some(preference) = PowerPreference::from_env() {
            setup.power_preference = preference;
        } else {
            setup.power_preference = PowerPreference::None;
            setup.native_adapter_selector = Some(Arc::new(|adapters, surface| {
                adapters
                    .iter()
                    .filter(|adapter| surface.is_none_or(|s| adapter.is_surface_supported(s)))
                    .min_by_key(|adapter| adapter.get_info().device_type == DeviceType::Cpu)
                    .cloned()
                    .ok_or_else(|| "no graphics adapter can draw to the window".to_owned())
            }));
        }
    }
    config
}

#[cfg(any(windows, target_os = "macos"))]
fn wgpu_options() -> eframe::WgpuConfiguration {
    eframe::WgpuConfiguration::default()
}

fn create_tray(sender: Sender<TrayEvent>) -> Result<tray_icon::TrayIcon, String> {
    use tray_icon::{
        menu::{Menu, MenuEvent, MenuItem},
        Icon, TrayIconBuilder,
    };

    let menu = Menu::new();
    let show_item = MenuItem::with_id(TRAY_SHOW, "Show Viper Mixer", true, None);
    let quit_item = MenuItem::with_id(TRAY_QUIT, "Quit", true, None);
    menu.append_items(&[&show_item, &quit_item])
        .map_err(|error| error.to_string())?;

    let show_id = show_item.id().clone();
    let quit_id = quit_item.id().clone();
    MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
        if event.id == show_id {
            let _ = sender.send(TrayEvent::Show);
        } else if event.id == quit_id {
            let _ = sender.send(TrayEvent::Quit);
        }
    }));

    let decoded = image::load_from_memory(LOGO).expect("bundled logo must be valid");
    let image = decoded.thumbnail(96, 96).into_rgba8();
    let (width, height) = image.dimensions();
    let icon =
        Icon::from_rgba(image.into_raw(), width, height).map_err(|error| error.to_string())?;

    TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_tooltip("Viper Mixer")
        .with_icon(icon)
        .build()
        .map_err(|error| error.to_string())
}

impl Drop for MixerApp {
    fn drop(&mut self) {
        // Signal engine to shutdown
        self.engine.shutdown();
        if let Err(error) = self.config.save() {
            eprintln!("Could not save mixer settings during shutdown: {error}");
        }
    }
}
