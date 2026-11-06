//! The window: a dark console with five channel strips, each with an assignment plate, a fader,
//! a level readout, a mute button and the controller bindings it learned.

use crate::config::{FADER_COUNT, INPUT, MIDI_DISCONNECTED, MuteMode, OUTPUT, STEAM_ALL};
use crate::engine::Engine;
use crate::mixer::{Health, Learn, Menu, Mixer, port_title};
use crate::midi::{self, Control};
use crate::stream::{AudioStream, Device};
use rune::{Align, AppSpec, Color, Cx, Drag, El, Key, KeyEvent, Phase, ScrollSpec, Shadow, Side, TextStyle, Tx, col, el, row, text};
use std::time::{Duration, Instant};

const STRIP_W: f32 = 136.0;
const FADER_H: f32 = 220.0;
const CAP_W: f32 = 46.0;
const CAP_H: f32 = 26.0;
const TRAVEL: f32 = FADER_H - CAP_H;
const SCALE_W: f32 = 30.0;
/// How long a strip's controller light stays on after a message.
const ACTIVITY: Duration = Duration::from_millis(140);
/// The fader scale, in dB.
const MARKS: [f32; 6] = [0.0, -6.0, -12.0, -24.0, -36.0, -48.0];

/// Patchwork's palette, one colour per strip.
const ACCENTS: [&str; FADER_COUNT] = ["#5BC0EB", "#9B5DE5", "#FF7EB3", "#FEE440", "#00F5D4"];

fn c(hex: &str) -> Color {
    rune::color::hex(hex).unwrap_or(Color::BLACK)
}

fn style(size: f32, weight: f32) -> TextStyle {
    TextStyle::new(size, weight)
}

fn mono(size: f32, weight: f32) -> TextStyle {
    TextStyle::new(size, weight).mono()
}

fn label(content: impl Into<String>, size: f32, weight: f32, color: &str) -> El {
    text(content.into()).font(style(size, weight)).color(c(color))
}

fn led(health: Option<Health>, size: f32) -> El {
    let color = match health {
        Some(Health::Good) => "#3EF08A",
        Some(Health::Waiting) => "#FFB547",
        Some(Health::Bad) => "#FF5A6A",
        None => "#3A3F4A",
    };
    let mut dot = el().size(size, size).radius(size / 2.0).fill(c(color)).border(1.0, Color::BLACK.alpha(0.45));
    if health.is_some() {
        dot = dot.shadow(0.0, size * 1.6, c(color).alpha(0.7));
    }
    dot.a11y_hidden()
}

/// A level as the scale shows it: the cubic volume PipeWire uses, in dB.
pub(crate) fn decibels(volume: f32) -> String {
    if volume <= 0.0001 { "−∞ dB".into() } else { format!("{:.1} dB", 60.0 * volume.log10()).replace('-', "−") }
}

fn volume_at(decibels: f32) -> f32 {
    10f32.powf(decibels / 60.0)
}

fn cap_top(volume: f32) -> f32 {
    (1.0 - volume) * TRAVEL
}

pub(crate) fn spec() -> AppSpec<Mixer> {
    AppSpec {
        min_size: Some((780.0, 580.0)),
        setup: Some(|s, waker| {
            if rune::tooling::headless_run().is_none() {
                s.attach(Engine::start(waker));
            }
        }),
        poll: Some(poll),
        on_key: Some(on_key),
        scenes: &[
            ("default", demo),
            ("assign-menu", |s| {
                demo(s);
                s.menu = Some(Menu::Assign(1));
            }),
            ("midi-menu", |s| {
                demo(s);
                s.menu = Some(Menu::Midi);
            }),
            ("learning", |s| {
                demo(s);
                s.learning = Some(Learn::Mute(2));
            }),
            ("disconnected", |s| {
                s.pipewire_health = Health::Bad;
                s.pipewire_status = "PipeWire unavailable · pw-dump isn't installed; install the PipeWire tools".into();
                s.midi_status = "No MIDI device · plug one in".into();
            }),
        ],
        ..AppSpec::new("Viper Mixer", (780.0, 580.0), || Mixer::new(Default::default()), view)
    }
}

fn poll(s: &mut Mixer, _: f64, tx: &mut Tx) -> bool {
    let now = Instant::now();
    let mut changed = s.drain(now);
    let mut next = s.tick(now);
    for slot in &mut s.touched_at {
        if let Some(at) = *slot {
            match ACTIVITY.checked_sub(now.duration_since(at)) {
                Some(left) if !left.is_zero() => next = next.min(left),
                _ => {
                    *slot = None;
                    changed = true;
                }
            }
        }
    }
    tx.wake_after(next);
    changed
}

fn on_key(s: &mut Mixer, key: &KeyEvent, _: &mut Tx) -> bool {
    if key.key == Key::Escape && (s.learning.is_some() || s.menu.is_some()) {
        s.learning = None;
        s.menu = None;
        return true;
    }
    false
}

/// Closing the window hides it to the tray, unless that's off or there's no tray.
pub(crate) fn on_close(s: &mut Mixer, tx: &mut Tx) {
    s.menu = None;
    s.learning = None;
    s.save_now();
    if !(s.config.close_to_tray && s.tray_available) {
        tx.quit();
    }
}

fn view(s: &Mixer, _: &mut Cx<Mixer>) -> El {
    let mut strips = row().key("strips").gap(10.0).px(16.0).py(14.0).grow(1.0).justify_center().items_start();
    for index in 0..FADER_COUNT {
        strips = strips.child(strip(s, index));
    }
    let mut root = col()
        .key("window")
        .role("window")
        .w_full()
        .h_full()
        .gradient(c("#1F2228"), c("#121317"))
        .child(header(s))
        .child(el().key("rule").h(1.0).w_full().fill(Color::BLACK.alpha(0.6)))
        .child(strips)
        .child(footer(s));
    if let Some(menu) = s.menu {
        root = root
            .child(el().key("scrim").role("none").absolute(0.0, 0.0).w_full().h_full().on_press(|s: &mut Mixer| s.menu = None))
            .child(menu_panel(s, menu));
    }
    root
}

fn logo() -> El {
    // The app icon, drawn small: a console with a seam and three faders.
    let mut faders = row().key("faders").gap(4.0).absolute(7.0, 14.0);
    for (index, top) in [9.0, 3.0, 6.0].into_iter().enumerate() {
        faders = faders.child(
            el().key(format!("slot{index}"))
                .size(4.0, 15.0)
                .radius(2.0)
                .fill(c("#2D3047"))
                .child(el().size(6.0, 4.0).radius(1.5).absolute(-1.0, top).fill(Color::WHITE)),
        );
    }
    el().key("logo")
        .size(34.0, 34.0)
        .radius(9.0)
        .fill(c("#9B5DE5"))
        .clip_rounded(9.0)
        .shadow(2.0, 8.0, Color::BLACK.alpha(0.5))
        .child(el().size(34.0, 10.0).absolute(0.0, 0.0).fill(c("#5BC0EB")))
        .child(el().size(34.0, 1.0).absolute(0.0, 10.0).fill(Color::WHITE.alpha(0.75)))
        .child(faders)
        .a11y_hidden()
}

fn header(s: &Mixer) -> El {
    let port = if s.connected_port.is_empty() {
        match s.config.midi_port.as_str() {
            "" => "Automatic".to_owned(),
            MIDI_DISCONNECTED => "Off".to_owned(),
            saved => port_title(saved).to_owned(),
        }
    } else {
        port_title(&s.connected_port).to_owned()
    };
    let open = s.menu == Some(Menu::Midi);
    row()
        .key("header")
        .h(60.0)
        .w_full()
        .px(18.0)
        .gap(12.0)
        .center_y()
        .gradient(c("#2A2E36"), c("#1E2127"))
        .child(logo())
        .child(
            col()
                .key("title")
                .gap(1.0)
                .child(label("Viper Mixer", 17.0, 700.0, "#EEF0F5"))
                .child(label("MIDI fader controller for PipeWire", 11.0, 500.0, "#7D8494")),
        )
        .child(el().grow(1.0))
        .child(
            row()
                .key("midi-button")
                .role("button")
                .label("MIDI device")
                .focusable(true)
                .w(250.0)
                .h(34.0)
                .px(10.0)
                .gap(8.0)
                .center_y()
                .radius(7.0)
                .gradient(c("#16181D"), c("#1B1E24"))
                .border(1.0, if open { c("#5BC0EB") } else { Color::BLACK.alpha(0.7) })
                .box_shadow(Shadow { x: 0.0, y: 1.0, blur: 0.0, spread: 0.0, color: Color::WHITE.alpha(0.06), inset: false })
                .hover(Color::WHITE.alpha(0.04))
                .on_press(|s: &mut Mixer| s.menu = if s.menu == Some(Menu::Midi) { None } else { Some(Menu::Midi) })
                .child(led(Some(s.midi_health()), 8.0))
                .child(label("MIDI", 10.0, 700.0, "#7D8494"))
                .child(label(port, 12.0, 600.0, "#E4E7EE").grow(1.0).truncate())
                .child(label("▾", 11.0, 600.0, "#7D8494")),
        )
        .child(
            row()
                .key("pipewire")
                .gap(7.0)
                .center_y()
                .child(led(Some(s.pipewire_health), 8.0))
                .child(label("PipeWire", 11.0, 600.0, "#9AA1B0")),
        )
}

fn strip(s: &Mixer, index: usize) -> El {
    let accent = c(ACCENTS[index]);
    let volume = s.config.fader_volumes[index];
    let lit = s.touched_at[index].is_some_and(|at| at.elapsed() < ACTIVITY);
    col()
        .key(format!("strip-{index}"))
        .w(STRIP_W)
        .padding(10.0)
        .gap(9.0)
        .radius(10.0)
        .gradient(c("#2B2F37"), c("#1C1F25"))
        .border_gradient(1.0, Color::WHITE.alpha(0.10), Color::BLACK.alpha(0.6))
        .shadow(4.0, 14.0, Color::BLACK.alpha(0.55))
        .child(plate(s, index, accent))
        .child(
            row()
                .key("fader-bay")
                .h(FADER_H + 12.0)
                .w_full()
                .py(6.0)
                .radius(7.0)
                .fill(Color::BLACK.alpha(0.18))
                .justify_center()
                .child(scale())
                .child(fader(index, volume, accent)),
        )
        .child(readout(index, volume, accent, lit))
        .child(mute_button(s, index))
        .child(binding_chip(s, index, false, accent))
        .child(binding_chip(s, index, true, accent))
}

fn plate(s: &Mixer, index: usize, accent: Color) -> El {
    let health = if s.is_live(index) {
        Some(Health::Good)
    } else if s.is_present(index) {
        Some(Health::Waiting)
    } else {
        None
    };
    let open = s.menu == Some(Menu::Assign(index));
    col()
        .key(format!("assign-{index}"))
        .role("button")
        .label(format!("Strip {} assignment", index + 1))
        .focusable(true)
        .w_full()
        .h(46.0)
        .px(8.0)
        .py(6.0)
        .gap(2.0)
        .radius(6.0)
        .gradient(c("#15171C"), c("#1A1D22"))
        .border(1.0, if open { accent } else { Color::BLACK.alpha(0.7) })
        .hover(Color::WHITE.alpha(0.04))
        .on_press(move |s: &mut Mixer| {
            s.menu = if s.menu == Some(Menu::Assign(index)) { None } else { Some(Menu::Assign(index)) };
        })
        .child(
            row()
                .gap(6.0)
                .center_y()
                .child(led(health, 7.0))
                .child(label(s.strip_title(index), 12.0, 650.0, "#EEF0F5").grow(1.0).truncate())
                .child(label("▾", 10.0, 600.0, "#6E7584")),
        )
        .child(label(s.strip_detail(index), 10.0, 500.0, "#7D8494").truncate())
        .child(el().size(18.0, 2.0).absolute(8.0, 0.0).radius(1.0).fill(accent))
}

fn scale() -> El {
    let mut scale = el().key("scale").size(SCALE_W, FADER_H).a11y_hidden();
    for mark in MARKS {
        let y = cap_top(volume_at(mark)) + CAP_H / 2.0;
        let name = if mark == 0.0 { "0".to_owned() } else { format!("{}", mark as i32).replace('-', "−") };
        scale = scale
            .child(
                text(name)
                    .font(mono(8.5, 500.0))
                    .color(c(if mark == 0.0 { "#C9CEDA" } else { "#6E7584" }))
                    .w(SCALE_W - 9.0)
                    .align(Align::End)
                    .absolute(0.0, y - 6.0),
            )
            .child(el().size(5.0, 1.0).absolute(SCALE_W - 6.0, y).fill(c("#6E7584")));
    }
    scale.child(text("−∞").font(mono(8.5, 500.0)).color(c("#6E7584")).w(SCALE_W - 9.0).align(Align::End).absolute(0.0, FADER_H - CAP_H / 2.0 - 6.0))
}

fn fader(index: usize, volume: f32, accent: Color) -> El {
    let top = cap_top(volume);
    let groove = 6.0;
    let to_volume = |y: f32| (1.0 - (y - CAP_H / 2.0) / TRAVEL).clamp(0.0, 1.0);
    let mut cap = el()
        .key("cap")
        .size(CAP_W, CAP_H)
        .absolute(0.0, top)
        .radius(4.0)
        .gradient(c("#EEF1F6"), c("#8E95A2"))
        .border(1.0, Color::BLACK.alpha(0.55))
        .shadow(4.0, 9.0, Color::BLACK.alpha(0.6))
        .a11y_hidden();
    for offset in [-7.0, -4.0, 4.0, 7.0] {
        cap = cap.child(el().size(CAP_W - 14.0, 1.0).absolute(6.0, CAP_H / 2.0 + offset - 0.5).fill(Color::BLACK.alpha(0.22)));
    }
    cap = cap.child(el().size(CAP_W - 8.0, 2.0).absolute(3.0, CAP_H / 2.0 - 2.0).radius(1.0).fill(accent));
    el().key(format!("fader-{index}"))
        .role("slider")
        .label(format!("Strip {} level", index + 1))
        .range_value(f64::from(volume), 0.0, 1.0)
        .range_step(0.01)
        .a11y_value(decibels(volume))
        .on_step(move |s: &mut Mixer, steps, _| s.set_volume(index, s.config.fader_volumes[index] + steps as f32 * 0.01))
        .on_step_limit(move |s: &mut Mixer, max, _| s.set_volume(index, if max { 1.0 } else { 0.0 }))
        .size(CAP_W, FADER_H)
        .ml(4.0)
        .on_drag(move |s: &mut Mixer, d: &mut Drag, tx| match d.phase {
            Phase::Down => {
                if d.clicks == 2 {
                    // A double press puts the fader back at unity.
                    s.set_volume(index, 1.0);
                }
                let top = cap_top(s.config.fader_volumes[index]);
                if !(top..top + CAP_H).contains(&d.y) {
                    // Pressed the groove: the cap jumps there and drags from it.
                    s.set_volume(index, to_volume(d.y));
                }
                d.data = s.config.fader_volumes[index];
            }
            Phase::Move => {
                s.set_volume(index, d.data - (d.y - d.start_y) / TRAVEL);
                tx.instant();
            }
            Phase::Up => {}
        })
        .child(
            el().key("groove")
                .size(groove, TRAVEL)
                .absolute((CAP_W - groove) / 2.0, CAP_H / 2.0)
                .radius(groove / 2.0)
                .fill(c("#08090B"))
                .box_shadow(Shadow { x: 0.0, y: 1.0, blur: 2.0, spread: 0.0, color: Color::BLACK, inset: true })
                .border(1.0, Color::WHITE.alpha(0.05))
                .a11y_hidden(),
        )
        .child(
            el().key("fill")
                .size(groove - 2.0, (FADER_H - CAP_H / 2.0 - (top + CAP_H / 2.0)).max(0.0))
                .absolute((CAP_W - groove) / 2.0 + 1.0, top + CAP_H / 2.0)
                .radius(groove / 2.0)
                .gradient(accent, accent.alpha(0.35))
                .shadow(0.0, 10.0, accent.alpha(0.55))
                .a11y_hidden(),
        )
        .child(cap)
}

fn readout(index: usize, volume: f32, accent: Color, lit: bool) -> El {
    row()
        .key(format!("readout-{index}"))
        .w_full()
        .h(32.0)
        .px(8.0)
        .center_y()
        .radius(5.0)
        .fill(c("#0A0E0D"))
        .border(1.0, Color::BLACK)
        .box_shadow(Shadow { x: 0.0, y: 2.0, blur: 6.0, spread: 0.0, color: Color::BLACK.alpha(0.9), inset: true })
        .child(text(decibels(volume)).font(mono(13.0, 600.0)).color(accent).grow(1.0))
        .child(
            col()
                .items_end()
                .gap(3.0)
                .child(text(format!("{:.0}%", volume * 100.0)).font(mono(9.0, 500.0)).color(c("#6E7584")))
                .child(led(lit.then_some(Health::Good), 5.0)),
        )
        .a11y_hidden()
}

fn mute_button(s: &Mixer, index: usize) -> El {
    let muted = s.muted[index];
    let mut button = row()
        .key(format!("mute-{index}"))
        .role("switch")
        .label(format!("Mute strip {}", index + 1))
        .checked(muted)
        .focusable(true)
        .w_full()
        .h(30.0)
        .center()
        .radius(6.0)
        .on_press(move |s: &mut Mixer| s.set_muted(index, !s.muted[index]));
    button = if muted {
        button
            .gradient(c("#FF6B7A"), c("#C7303F"))
            .border(1.0, c("#FF9AA5").alpha(0.6))
            .shadow(0.0, 16.0, c("#FF4D5E").alpha(0.55))
            .child(label("MUTED", 11.0, 800.0, "#FFFFFF").a11y_hidden())
    } else {
        button
            .gradient(c("#353A44"), c("#262A31"))
            .border_gradient(1.0, Color::WHITE.alpha(0.10), Color::BLACK.alpha(0.6))
            .shadow(2.0, 4.0, Color::BLACK.alpha(0.5))
            .hover(Color::WHITE.alpha(0.05))
            .child(label("MUTE", 11.0, 800.0, "#9AA1B0").a11y_hidden())
    };
    button
}

fn binding_chip(s: &Mixer, index: usize, mute: bool, accent: Color) -> El {
    let learn = if mute { Learn::Mute(index) } else { Learn::Fader(index) };
    let learning = s.learning == Some(learn);
    let bound = if mute { s.config.mute_controls[index] } else { s.config.fader_controls[index] };
    let value = if learning {
        "LISTENING…".to_owned()
    } else {
        bound.as_ref().map_or_else(|| "LEARN".to_owned(), Control::label)
    };
    row()
        .key(format!("{}-learn-{index}", if mute { "mute" } else { "fader" }))
        .role("button")
        .label(format!("Learn the {} control for strip {}", if mute { "mute" } else { "fader" }, index + 1))
        .help("Press, then move a fader or press a button on the controller. Right-click to unbind.")
        .focusable(true)
        .w_full()
        .h(21.0)
        .px(6.0)
        .gap(5.0)
        .center_y()
        .radius(4.0)
        .fill(if learning { accent.alpha(0.18) } else { Color::BLACK.alpha(0.25) })
        .border(1.0, if learning { accent } else { Color::WHITE.alpha(0.05) })
        .hover(Color::WHITE.alpha(0.05))
        .on_press(move |s: &mut Mixer| s.learning = if s.learning == Some(learn) { None } else { Some(learn) })
        .on_secondary_press(move |s: &mut Mixer, _, _| {
            let list = if mute { &mut s.config.mute_controls } else { &mut s.config.fader_controls };
            list[index] = None;
            s.learning = None;
            s.schedule_save();
        })
        .child(text(if mute { "MUTE" } else { "FADER" }).font(mono(8.0, 700.0)).color(c("#6E7584")).a11y_hidden())
        .child(
            text(value)
                .font(mono(8.5, 600.0))
                .color(if learning { accent } else if bound.is_some() { c("#C9CEDA") } else { c("#6E7584") })
                .grow(1.0)
                .align(Align::End)
                .truncate()
                .a11y_hidden(),
        )
}

fn footer(s: &Mixer) -> El {
    let (status, color) = match &s.error {
        Some(error) => (error.clone(), "#FF8A96"),
        None => (format!("{}  ·  {}", s.pipewire_status, s.midi_status), "#7D8494"),
    };
    let mode = |name: &'static str, mode: MuteMode| {
        let on = s.config.mute_mode == mode;
        el().key(name)
            .role("radio")
            .label(format!("{name} mute buttons"))
            .checked(on)
            .focusable(true)
            .h(22.0)
            .px(9.0)
            .center()
            .radius(4.0)
            .fill(if on { c("#3A3F4A") } else { Color::TRANSPARENT })
            .hover(Color::WHITE.alpha(0.05))
            .on_press(move |s: &mut Mixer| {
                s.config.mute_mode = mode;
                s.schedule_save();
            })
            .child(label(name, 10.0, 700.0, if on { "#EEF0F5" } else { "#7D8494" }).a11y_hidden())
    };
    let tray = s.config.close_to_tray;
    row()
        .key("footer")
        .h(40.0)
        .w_full()
        .px(18.0)
        .gap(10.0)
        .center_y()
        .fill(Color::BLACK.alpha(0.25))
        .child(label(status, 11.0, 500.0, color).grow(1.0).truncate())
        .child(
            row()
                .key("tray")
                .role("switch")
                .label("Close to tray")
                .help(if s.tray_available { "Closing the window keeps the mixer running in the tray." } else { "No system tray, so closing the window quits." })
                .checked(tray)
                .focusable(true)
                .h(22.0)
                .px(8.0)
                .gap(6.0)
                .center_y()
                .radius(4.0)
                .hover(Color::WHITE.alpha(0.05))
                .on_press(|s: &mut Mixer| {
                    s.config.close_to_tray = !s.config.close_to_tray;
                    s.schedule_save();
                })
                .child(led(tray.then_some(Health::Good), 6.0))
                .child(label("Close to tray", 10.0, 700.0, if s.tray_available { "#9AA1B0" } else { "#5A6070" }).a11y_hidden()),
        )
        .child(
            row()
                .key("mute-mode")
                .role("radiogroup")
                .label("Mute buttons")
                .padding(2.0)
                .radius(6.0)
                .fill(Color::BLACK.alpha(0.35))
                .child(mode("Latch", MuteMode::Latch))
                .child(mode("Toggle", MuteMode::Toggle)),
        )
        .child(
            el().key("rescan")
                .role("button")
                .label("Rescan audio and MIDI")
                .focusable(true)
                .h(24.0)
                .px(10.0)
                .center()
                .radius(5.0)
                .gradient(c("#353A44"), c("#262A31"))
                .border_gradient(1.0, Color::WHITE.alpha(0.10), Color::BLACK.alpha(0.6))
                .hover(Color::WHITE.alpha(0.05))
                .on_press(|s: &mut Mixer| {
                    s.error = None;
                    s.rescan();
                    s.refresh_midi();
                })
                .child(label("Rescan", 10.0, 700.0, "#C9CEDA").a11y_hidden()),
        )
}

/// A menu row: its id, label, light, and whether it's the current choice.
type MenuItem = (String, String, Option<Health>, bool);

fn menu_panel(s: &Mixer, menu: Menu) -> El {
    let (anchor, accent, items): (String, Color, Vec<MenuItem>) = match menu {
        Menu::Assign(index) => {
            let current = s.assignment(index).to_owned();
            let items = s
                .assignment_options(index)
                .into_iter()
                .map(|(id, name, running)| {
                    let health = match id.as_str() {
                        "" | STEAM_ALL | OUTPUT | INPUT => None,
                        _ if running => Some(Health::Good),
                        _ => None,
                    };
                    let chosen = id == current;
                    (id, name, health, chosen)
                })
                .collect();
            (format!("assign-{index}"), c(ACCENTS[index]), items)
        }
        Menu::Midi => {
            let mut items = vec![(String::new(), "Automatic".to_owned(), None, s.config.midi_port.is_empty())];
            for port in &s.midi_ports {
                let stable = midi::stable_port_name(port).to_owned();
                let connected = *port == s.connected_port;
                let chosen = stable == s.config.midi_port;
                items.push((stable, port_title(port).to_owned(), connected.then_some(Health::Good), chosen));
            }
            let saved = &s.config.midi_port;
            if !saved.is_empty() && saved != MIDI_DISCONNECTED && !items.iter().any(|(id, ..)| id == saved) {
                items.push((saved.clone(), format!("{} (unplugged)", port_title(saved)), Some(Health::Waiting), true));
            }
            items.push((MIDI_DISCONNECTED.to_owned(), "Off".to_owned(), None, s.config.midi_port == MIDI_DISCONNECTED));
            ("midi-button".to_owned(), c("#5BC0EB"), items)
        }
    };
    let mut list = col().key("items").role("menu").gap(1.0).padding(4.0);
    for (position, (id, name, health, chosen)) in items.into_iter().enumerate() {
        let id = std::rc::Rc::new(id);
        list = list.child(
            row()
                .key(format!("item-{position}"))
                .role("menuitem")
                .label(name.clone())
                .checked(chosen)
                .focusable(true)
                .h(28.0)
                .px(8.0)
                .gap(8.0)
                .center_y()
                .radius(5.0)
                .fill(if chosen { accent.alpha(0.16) } else { Color::TRANSPARENT })
                .hover(Color::WHITE.alpha(0.07))
                .on_press(move |s: &mut Mixer| {
                    match menu {
                        Menu::Assign(index) => s.assign(index, &id),
                        Menu::Midi => s.choose_port(&id),
                    }
                    s.menu = None;
                })
                .child(led(health, 6.0))
                .child(label(name, 12.0, if chosen { 650.0 } else { 500.0 }, if chosen { "#FFFFFF" } else { "#C9CEDA" }).grow(1.0).truncate().a11y_hidden())
                .child(text(if chosen { "●" } else { "" }).font(style(8.0, 700.0)).color(accent).a11y_hidden()),
        );
    }
    col()
        .key("menu")
        .w(260.0)
        .max_h(340.0)
        .radius(8.0)
        .fill(c("#23262D"))
        .border(1.0, Color::BLACK.alpha(0.8))
        .shadow(10.0, 30.0, Color::BLACK.alpha(0.6))
        .clip_rounded(8.0)
        .child(list.scroll(ScrollSpec { stick_bottom: false }).shrink().min_h(0.0))
        .anchored_fit(anchor, Side::Bottom, Align::Start, 6.0)
        .modal(true, |s: &mut Mixer| s.menu = None)
}

/// The state the snapshots show.
fn demo(s: &mut Mixer) {
    let stream = |node_id, name: &str, binary: &str, steam, running| AudioStream {
        node_id,
        name: name.into(),
        app_name: name.into(),
        app_id: String::new(),
        binary: binary.into(),
        description: String::new(),
        identity: format!("stream-app:{}::", name.to_lowercase()),
        is_steam_game: steam,
        running,
    };
    s.graph.streams = vec![
        stream(41, "Spotify", "spotify", false, true),
        stream(57, "Hades II", "Hades2.exe", true, true),
        stream(63, "Firefox", "firefox", false, false),
        stream(70, "Discord", "Discord", false, true),
    ];
    s.graph.output = Some(Device { description: "Scarlett 2i2 USB".into(), running: true });
    s.graph.input = Some(Device { description: "Scarlett 2i2 USB".into(), running: false });
    let keys = ["", "stream-app:spotify::", "", "stream-app:discord::", ""];
    let names = [OUTPUT, "Spotify", STEAM_ALL, "Discord", INPUT];
    for index in 0..FADER_COUNT {
        s.config.applications[index] = names[index].into();
        s.config.stream_keys[index] = keys[index].into();
    }
    s.config.fader_volumes = vec![0.82, 0.64, 0.9, 0.45, 0.7];
    s.config.mute_controls[1] = Some(Control { channel: Some(0), ..Control::cc(48) });
    s.muted[4] = true;
    s.midi_ports = vec!["Midi Through:Midi Through Port-0 14:0".into(), "nanoKONTROL2:nanoKONTROL2 _ CTRL 24:0".into()];
    s.connected_port = s.midi_ports[1].clone();
    s.midi_status = "Connected · nanoKONTROL2 _ CTRL".into();
    s.pipewire_health = Health::Good;
    s.pipewire_status = "4 apps playing through PipeWire".into();
    s.tray_available = true;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levels_read_in_decibels() {
        assert_eq!(decibels(1.0), "0.0 dB");
        assert_eq!(decibels(0.5), "−18.1 dB");
        assert_eq!(decibels(0.0), "−∞ dB");
        assert!((volume_at(-18.0) - 0.501).abs() < 0.001);
    }
}
