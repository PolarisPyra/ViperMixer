use crate::{
    config::{Config, FADER_COUNT, MIDI_DISCONNECTED, STEAM_ALL},
    engine::{Engine, EngineCommand, EngineEvent},
    stream::AudioStream,
};
use eframe::egui;
use midir::{Ignore, MidiInput, MidiInputConnection};
use std::{
    collections::HashSet,
    sync::mpsc::{self, Receiver, Sender},
    time::{Duration, Instant},
};

const LOGO: &[u8] = include_bytes!("../assets/logo.png");
const TRAY_SHOW: &str = "show";
const TRAY_QUIT: &str = "quit";

enum UiEvent {
    Midi(Vec<u8>),
}

enum TrayEvent {
    Ready,
    Show,
    Quit,
    Failed(String),
}

struct MixerApp {
    config: Config,
    streams: Vec<AudioStream>,
    midi_ports: Vec<String>,
    connection: Option<MidiInputConnection<()>>,
    connected_port: String,
    engine: Engine,
    midi_sender: Sender<UiEvent>,
    midi_receiver: Receiver<UiEvent>,
    tray_receiver: Receiver<TrayEvent>,
    tray_available: bool,
    quit_requested: bool,
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
    fn new(ctx: egui::Context) -> Self {
        let engine = Engine::start();
        let (midi_sender, midi_receiver) = mpsc::channel();
        let (tray_sender, tray_receiver) = mpsc::channel();
        start_tray_thread(tray_sender, ctx);
        let now = Instant::now();
        let mut app = Self {
            config: Config::load(),
            streams: Vec::new(),
            midi_ports: Vec::new(),
            connection: None,
            connected_port: String::new(),
            engine,
            midi_sender,
            midi_receiver,
            tray_receiver,
            tray_available: false,
            quit_requested: false,
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
        self.connection = None;
        self.connected_port.clear();
        self.status = "Refreshing MIDI devices…".into();
        self.last_connect_attempt = Instant::now() - Duration::from_secs(3);
        self.request_ports();
        self.connect_midi();
    }

    fn update_ports(&mut self, mut ports: Vec<String>) {
        ports.sort_by_key(|port| port.to_lowercase());
        self.midi_ports = ports;
        if !self.connected_port.is_empty() && !self.midi_ports.contains(&self.connected_port) {
            self.connection = None;
            self.connected_port.clear();
            self.status = "MIDI device disconnected · reconnecting".into();
        }
        if self.connection.is_none() && self.config.midi_port != MIDI_DISCONNECTED {
            self.connect_midi();
        }
    }

    fn connect_midi(&mut self) {
        self.last_connect_attempt = Instant::now();
        if self.config.midi_port == MIDI_DISCONNECTED {
            self.connection = None;
            self.connected_port.clear();
            self.status = "MIDI input disconnected".into();
            return;
        }
        if !self.config.midi_port.is_empty() && !self.midi_ports.contains(&self.config.midi_port) {
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
            self.connection = None;
            self.connected_port.clear();
            self.status = "No MIDI input found · reconnecting automatically".into();
            return;
        };
        let mut input = match MidiInput::new("Viper Mixer") {
            Ok(input) => input,
            Err(error) => {
                self.status = format!("MIDI unavailable · {error}");
                return;
            }
        };
        input.ignore(Ignore::None);
        let Some(port) = input
            .ports()
            .into_iter()
            .find(|port| input.port_name(port).ok().as_deref() == Some(port_name.as_str()))
        else {
            self.status = "MIDI device changed · retrying".into();
            return;
        };
        let sender = self.midi_sender.clone();
        match input.connect(
            &port,
            "viper-midi-mixer-input",
            move |_timestamp, message, _| {
                let _ = sender.send(UiEvent::Midi(message.to_vec()));
            },
            (),
        ) {
            Ok(connection) => {
                self.connection = Some(connection);
                self.connected_port.clone_from(&port_name);
                self.status = format!("Connected · {port_name}");
            }
            Err(error) => {
                self.status = format!("Could not connect to {port_name} · {error} · retrying")
            }
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
                    if ports != self.midi_ports {
                        self.update_ports(ports);
                    }
                }
                EngineEvent::MidiPorts(Err(error)) => {
                    self.pending_ports = false;
                    self.status = format!("MIDI unavailable · {error}");
                }
                EngineEvent::CommandError(error) => {
                    self.last_applied.clear();
                    self.error = Some(error);
                }
            }
        }
        while let Ok(UiEvent::Midi(data)) = self.midi_receiver.try_recv() {
            self.handle_midi(&data);
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

impl eframe::App for MixerApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.process_events();
        while let Ok(event) = self.tray_receiver.try_recv() {
            match event {
                TrayEvent::Ready => self.tray_available = true,
                TrayEvent::Show => {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
                    ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                    ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                }
                TrayEvent::Quit => {
                    self.quit_requested = true;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
                TrayEvent::Failed(error) => {
                    self.tray_available = false;
                    self.error = Some(format!("System tray unavailable: {error}"));
                }
            }
        }
        if self.tray_available && ctx.input(|input| input.viewport().minimized.unwrap_or(false)) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
        }
        if ctx.input(|input| input.viewport().close_requested()) {
            if self.tray_available && !self.quit_requested {
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
            }
        }
        if Instant::now().duration_since(self.last_stream_request) > Duration::from_secs(2) {
            self.request_streams();
        }
        if Instant::now().duration_since(self.last_ports_request) > Duration::from_secs(3) {
            self.request_ports();
        }
        if self.connection.is_none()
            && self.config.midi_port != MIDI_DISCONNECTED
            && Instant::now().duration_since(self.last_connect_attempt) >= Duration::from_secs(2)
        {
            self.connect_midi();
        }
        ctx.request_repaint_after(Duration::from_millis(120));
        style(ctx);
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(egui::Color32::from_rgb(32, 34, 36)).inner_margin(20.0))
            .show(ctx, |ui| {
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
                        self.config.midi_port = desired_port;
                        self.connection = None;
                        self.connected_port.clear();
                        self.connect_midi();
                        self.schedule_save();
                    }
                    if ui.button("Refresh").clicked() { self.refresh_midi(); }
                    let midi_status = if self.connection.is_some() {
                        format!("Connected: {}", self.connected_port)
                    } else {
                        self.status.clone()
                    };
                    ui.colored_label(if self.connection.is_some() { egui::Color32::from_rgb(55, 190, 112) } else { egui::Color32::from_rgb(213, 159, 76) }, midi_status);
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
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Viper Mixer")
            .with_app_id("viper-midi-mixer")
            .with_inner_size([986.0, 660.0])
            .with_min_inner_size([980.0, 620.0])
            .with_icon(icon),
        ..Default::default()
    };
    eframe::run_native(
        "Viper Mixer",
        options,
        Box::new(move |cc| {
            let mut style = (*cc.egui_ctx.style()).clone();
            style.spacing.item_spacing = egui::vec2(10.0, 8.0);
            cc.egui_ctx.set_style(style);
            Ok(Box::new(MixerApp::new(cc.egui_ctx.clone())))
        }),
    )
}

fn start_tray_thread(sender: Sender<TrayEvent>, repaint_ctx: egui::Context) {
    std::thread::Builder::new()
        .name("mixer-system-tray".into())
        .spawn(move || {
            if let Err(error) = gtk::init() {
                let _ = sender.send(TrayEvent::Failed(error.to_string()));
                return;
            }
            let tray_handle = std::rc::Rc::new(std::cell::RefCell::new(None));
            let handle_for_init = tray_handle.clone();
            let sender_for_init = sender.clone();
            gtk::glib::idle_add_local_once(move || {
                use tray_icon::{
                    menu::{Menu, MenuEvent, MenuItem},
                    Icon, TrayIconBuilder,
                };

                let menu = Menu::new();
                let show_item = MenuItem::with_id(TRAY_SHOW, "Show Viper Mixer", true, None);
                let quit_item = MenuItem::with_id(TRAY_QUIT, "Quit", true, None);
                if let Err(error) = menu.append_items(&[&show_item, &quit_item]) {
                    let _ = sender_for_init.send(TrayEvent::Failed(error.to_string()));
                    return;
                }
                let show_id = show_item.id().clone();
                let quit_id = quit_item.id().clone();
                let sender_for_events = sender_for_init.clone();
                let repaint_ctx = repaint_ctx.clone();
                MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
                    if event.id == show_id {
                        let _ = sender_for_events.send(TrayEvent::Show);
                    } else if event.id == quit_id {
                        let _ = sender_for_events.send(TrayEvent::Quit);
                    }
                    repaint_ctx.request_repaint();
                }));

                let decoded = image::load_from_memory(LOGO).expect("bundled logo must be valid");
                let image = decoded.thumbnail(96, 96).into_rgba8();
                let (width, height) = image.dimensions();
                let icon = match Icon::from_rgba(image.into_raw(), width, height) {
                    Ok(icon) => icon,
                    Err(error) => {
                        let _ = sender_for_init.send(TrayEvent::Failed(error.to_string()));
                        return;
                    }
                };
                match TrayIconBuilder::new()
                    .with_menu(Box::new(menu))
                    .with_tooltip("Viper Mixer")
                    .with_icon(icon)
                    .build()
                {
                    Ok(tray) => {
                        *handle_for_init.borrow_mut() = Some(tray);
                        let _ = sender_for_init.send(TrayEvent::Ready);
                    }
                    Err(error) => {
                        let _ = sender_for_init.send(TrayEvent::Failed(error.to_string()));
                    }
                }
            });
            gtk::main();
            drop(tray_handle);
        })
        .expect("could not start GTK tray event thread");
}

impl Drop for MixerApp {
    fn drop(&mut self) {
        if let Err(error) = self.config.save() {
            eprintln!("Could not save mixer settings during shutdown: {error}");
        }
    }
}
