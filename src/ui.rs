//! The window: five channel strips in a flat, plugin-style panel, each with what it drives, its
//! level, a fader, a mute button and the controller bindings it learned. Everything stretches with
//! the window; the faders take the height.

use crate::config::{FADER_COUNT, INPUT, MIDI_DISCONNECTED, MuteMode, OUTPUT, STEAM_ALL};
use crate::engine::Engine;
use crate::midi::{self, Control};
use crate::mixer::{Health, Learn, Menu, Mixer, port_title};
use crate::stream::{AudioStream, Device};
use rune::{Align, AppSpec, Color, Cx, Drag, El, Key, KeyEvent, Phase, ScrollSpec, Side, TextStyle, Tx, col, el, row, text};
use std::time::{Duration, Instant};

/// Space above and below the fader's travel.
const FADER_PAD: f32 = 8.0;
const HANDLE_W: f32 = 30.0;
const HANDLE_H: f32 = 10.0;
const TRACK_W: f32 = 4.0;
const SCALE_W: f32 = 24.0;
/// The fader's height before the first layout measures it.
const FADER_GUESS: f32 = 260.0;
/// Below this strip width the binding chips drop their "Fader" and "Mute" labels.
const COMPACT_STRIP: f32 = 108.0;
/// How long a strip's controller light stays on after a message.
const ACTIVITY: Duration = Duration::from_millis(140);
/// The fader scale, in dB; the short list is for a short window.
const MARKS: [f32; 6] = [0.0, -6.0, -12.0, -24.0, -36.0, -48.0];
const SHORT_MARKS: [f32; 3] = [0.0, -12.0, -48.0];

/// Patchwork's palette, one colour per strip.
const ACCENTS: [&str; FADER_COUNT] = ["#5BC0EB", "#9B5DE5", "#FF7EB3", "#FEE440", "#00F5D4"];

const BG: &str = "#15161A";
const PANEL: &str = "#1A1C20";
const HAIR: &str = "#2A2D33";
const TEXT: &str = "#E7E9EE";
const DIM: &str = "#8A909B";
const FAINT: &str = "#5B606B";
const RED: &str = "#E5484D";

fn c(hex: &str) -> Color {
    rune::color::hex(hex).unwrap_or(Color::BLACK)
}

fn label(content: impl Into<String>, size: f32, weight: f32, color: &str) -> El {
    text(content.into()).font(TextStyle::new(size, weight)).color(c(color))
}

fn mono(content: impl Into<String>, size: f32, color: Color) -> El {
    text(content.into()).font(TextStyle::new(size, 500.0).mono()).color(color)
}

fn health_color(health: Health) -> Color {
    c(match health {
        Health::Good => "#3DD68C",
        Health::Waiting => "#F5A524",
        Health::Bad => RED,
    })
}

fn dot(color: Option<Color>, size: f32) -> El {
    el().size(size, size).radius(size / 2.0).fill(color.unwrap_or(c(HAIR))).a11y_hidden()
}

/// A level as the scale shows it: the cubic volume PipeWire uses, in dB.
pub(crate) fn decibels(volume: f32) -> String {
    format!("{} dB", level_number(volume))
}

fn level_number(volume: f32) -> String {
    if volume <= 0.0001 { "−∞".into() } else { format!("{:.1}", 60.0 * volume.log10()).replace('-', "−") }
}

fn volume_at(decibels: f32) -> f32 {
    10f32.powf(decibels / 60.0)
}

/// Where the handle's centre sits for `volume` in a fader `height` tall.
fn handle_y(volume: f32, height: f32) -> f32 {
    FADER_PAD + HANDLE_H / 2.0 + (1.0 - volume) * travel(height)
}

fn travel(height: f32) -> f32 {
    (height - 2.0 * FADER_PAD - HANDLE_H).max(1.0)
}

pub(crate) fn spec() -> AppSpec<Mixer> {
    AppSpec {
        min_size: Some((560.0, 460.0)),
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
        ..AppSpec::new("Viper Mixer", (760.0, 560.0), || Mixer::new(Default::default()), view)
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

fn view(s: &Mixer, cx: &mut Cx<Mixer>) -> El {
    // Every fader is as tall as the first, and every strip as wide.
    let fader_h = cx.layout_size("fader-0").map_or(FADER_GUESS, |[_, h]| h);
    let compact = cx.layout_with("strip-0", |r| r[2] < COMPACT_STRIP).unwrap_or(false);
    let mut strips = row().key("strips").grow(1.0).w_full().px(6.0).min_h(0.0);
    for index in 0..FADER_COUNT {
        if index > 0 {
            strips = strips.child(el().key(format!("rule-{index}")).w(1.0).my(14.0).fill(c(HAIR)));
        }
        strips = strips.child(strip(s, index, fader_h, compact));
    }
    let mut root = col()
        .key("window")
        .role("window")
        .w_full()
        .h_full()
        .fill(c(BG))
        .child(header(s))
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
    let mut faders = row().key("faders").gap(3.0).absolute(5.0, 10.0);
    for (index, top) in [6.0, 2.0, 4.0].into_iter().enumerate() {
        faders = faders.child(
            el().key(format!("slot{index}"))
                .size(3.0, 11.0)
                .radius(1.5)
                .fill(c("#2D3047"))
                .child(el().size(5.0, 3.0).radius(1.0).absolute(-1.0, top).fill(Color::WHITE)),
        );
    }
    el().key("logo")
        .size(24.0, 24.0)
        .radius(6.0)
        .fill(c("#9B5DE5"))
        .clip_rounded(6.0)
        .child(el().size(24.0, 7.0).absolute(0.0, 0.0).fill(c("#5BC0EB")))
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
        .h(48.0)
        .w_full()
        .px(16.0)
        .gap(10.0)
        .center_y()
        .fill(c(PANEL))
        .child(logo())
        .child(label("Viper Mixer", 14.0, 650.0, TEXT))
        .child(el().grow(1.0))
        .child(
            row()
                .key("pipewire")
                .gap(6.0)
                .center_y()
                .help(s.pipewire_status.clone())
                .child(dot(Some(health_color(s.pipewire_health)), 6.0))
                .child(label("PipeWire", 11.0, 500.0, DIM)),
        )
        .child(
            row()
                .key("midi-button")
                .role("button")
                .label("MIDI device")
                .help(s.midi_status.clone())
                .focusable(true)
                .w(220.0)
                .h(30.0)
                .px(10.0)
                .gap(8.0)
                .center_y()
                .radius(6.0)
                .border(1.0, if open { c(ACCENTS[0]) } else { c(HAIR) })
                .hover(Color::WHITE.alpha(0.04))
                .on_press(|s: &mut Mixer| s.menu = if s.menu == Some(Menu::Midi) { None } else { Some(Menu::Midi) })
                .child(dot(Some(health_color(s.midi_health())), 6.0))
                .child(label("MIDI", 10.0, 600.0, FAINT))
                .child(label(port, 12.0, 500.0, TEXT).grow(1.0).truncate())
                .child(label("▾", 10.0, 500.0, FAINT)),
        )
}

fn strip(s: &Mixer, index: usize, fader_h: f32, compact: bool) -> El {
    let accent = c(ACCENTS[index]);
    let volume = s.config.fader_volumes[index];
    let lit = s.touched_at[index].is_some_and(|at| at.elapsed() < ACTIVITY);
    col()
        .key(format!("strip-{index}"))
        .w(0.0)
        .grow(1.0)
        .min_w(84.0)
        .px(10.0)
        .pt(12.0)
        .pb(12.0)
        .gap(10.0)
        .child(el().key("tag").h(2.0).w_full().radius(1.0).fill(if lit { accent } else { accent.alpha(0.45) }))
        .child(name_plate(s, index, accent))
        .child(
            row()
                .key("level")
                .px(6.0)
                .gap(4.0)
                .items_end()
                .child(text(level_number(volume)).font(TextStyle::new(22.0, 500.0).features("'tnum' 1")).color(c(TEXT)))
                .child(label("dB", 11.0, 500.0, FAINT).mb(3.0)),
        )
        .child(fader(index, volume, fader_h, accent))
        .child(mute_button(s, index))
        .child(
            col()
                .key("bindings")
                .gap(2.0)
                .child(binding_chip(s, index, false, accent, compact))
                .child(binding_chip(s, index, true, accent, compact)),
        )
}

fn name_plate(s: &Mixer, index: usize, accent: Color) -> El {
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
        .px(6.0)
        .py(5.0)
        .gap(2.0)
        .radius(6.0)
        .fill(if open { accent.alpha(0.12) } else { Color::TRANSPARENT })
        .hover(Color::WHITE.alpha(0.04))
        .on_press(move |s: &mut Mixer| {
            s.menu = if s.menu == Some(Menu::Assign(index)) { None } else { Some(Menu::Assign(index)) };
        })
        .child(
            row()
                .gap(6.0)
                .center_y()
                .child(dot(health.map(health_color), 6.0))
                .child(label(s.strip_title(index), 13.0, 600.0, TEXT).grow(1.0).truncate())
                .child(label("▾", 10.0, 500.0, FAINT)),
        )
        .child(label(s.strip_detail(index), 11.0, 400.0, FAINT).truncate())
}

fn scale(height: f32) -> El {
    let marks: &[f32] = if height < 200.0 { &SHORT_MARKS } else { &MARKS };
    let mut scale = el().key("scale").w(SCALE_W).h_full().a11y_hidden();
    for &mark in marks {
        let y = handle_y(volume_at(mark), height);
        let name = if mark == 0.0 { "0".to_owned() } else { format!("{}", mark as i32).replace('-', "−") };
        scale = scale
            .child(mono(name, 9.0, c(if mark == 0.0 { DIM } else { FAINT })).w(SCALE_W - 7.0).align(Align::End).absolute(0.0, y - 6.0))
            .child(el().size(4.0, 1.0).absolute(SCALE_W - 4.0, y).fill(c(HAIR)));
    }
    scale
}

fn fader(index: usize, volume: f32, height: f32, accent: Color) -> El {
    let y = handle_y(volume, height);
    let top = FADER_PAD + HANDLE_H / 2.0;
    let bottom = height - top;
    let track_x = (HANDLE_W - TRACK_W) / 2.0;
    let track = el()
        .key("track")
        .w(HANDLE_W)
        .h_full()
        .child(el().key("groove").size(TRACK_W, (bottom - top).max(0.0)).absolute(track_x, top).radius(TRACK_W / 2.0).fill(c(HAIR)))
        .child(el().key("fill").size(TRACK_W, (bottom - y).max(0.0)).absolute(track_x, y).radius(TRACK_W / 2.0).fill(accent))
        .child(
            el().key("handle")
                .size(HANDLE_W, HANDLE_H)
                .absolute(0.0, y - HANDLE_H / 2.0)
                .radius(3.0)
                .fill(c(TEXT))
                .shadow(1.0, 3.0, Color::BLACK.alpha(0.5)),
        )
        .a11y_hidden();
    // The whole width takes the drag, not just the handle.
    row()
        .key(format!("fader-{index}"))
        .role("slider")
        .label(format!("Strip {} level", index + 1))
        .range_value(f64::from(volume), 0.0, 1.0)
        .range_step(0.01)
        .a11y_value(decibels(volume))
        .on_step(move |s: &mut Mixer, steps, _| s.set_volume(index, s.config.fader_volumes[index] + steps as f32 * 0.01))
        .on_step_limit(move |s: &mut Mixer, max, _| s.set_volume(index, if max { 1.0 } else { 0.0 }))
        .w_full()
        .grow(1.0)
        .min_h(0.0)
        .justify_center()
        .on_drag(move |s: &mut Mixer, d: &mut Drag, tx| match d.phase {
            Phase::Down => {
                let at = handle_y(s.config.fader_volumes[index], d.h);
                if d.clicks == 2 {
                    // A double press puts the fader back at unity.
                    s.set_volume(index, 1.0);
                } else if (d.y - at).abs() > HANDLE_H {
                    // Pressed beside the handle: it jumps there and drags from it.
                    s.set_volume(index, 1.0 - (d.y - FADER_PAD - HANDLE_H / 2.0) / travel(d.h));
                }
                d.data = s.config.fader_volumes[index];
            }
            Phase::Move => {
                s.set_volume(index, d.data - (d.y - d.start_y) / travel(d.h));
                tx.instant();
            }
            Phase::Up => {}
        })
        .child(scale(height))
        .child(track)
        .child(el().w(SCALE_W))
}

fn mute_button(s: &Mixer, index: usize) -> El {
    let muted = s.muted[index];
    let button = row()
        .key(format!("mute-{index}"))
        .role("switch")
        .label(format!("Mute strip {}", index + 1))
        .checked(muted)
        .focusable(true)
        .w_full()
        .h(28.0)
        .center()
        .radius(6.0)
        .on_press(move |s: &mut Mixer| s.set_muted(index, !s.muted[index]));
    if muted {
        button.fill(c(RED)).child(label("Muted", 12.0, 600.0, "#FFFFFF").a11y_hidden())
    } else {
        button.border(1.0, c(HAIR)).hover(Color::WHITE.alpha(0.04)).child(label("Mute", 12.0, 500.0, DIM).a11y_hidden())
    }
}

fn binding_chip(s: &Mixer, index: usize, mute: bool, accent: Color, compact: bool) -> El {
    let learn = if mute { Learn::Mute(index) } else { Learn::Fader(index) };
    let learning = s.learning == Some(learn);
    let bound = if mute { s.config.mute_controls[index] } else { s.config.fader_controls[index] };
    let (value, color) = if learning {
        ("Listening…".to_owned(), accent)
    } else {
        match bound {
            Some(control) => (control.label(), c(DIM)),
            None => ("Learn".to_owned(), c(FAINT)),
        }
    };
    let mut chip = row()
        .key(format!("{}-learn-{index}", if mute { "mute" } else { "fader" }))
        .role("button")
        .label(format!("Learn the {} control for strip {}", if mute { "mute" } else { "fader" }, index + 1))
        .help("Press, then move a fader or press a button on the controller. Right-click to unbind.")
        .focusable(true)
        .w_full()
        .h(22.0)
        .px(6.0)
        .gap(4.0)
        .center_y()
        .radius(5.0)
        .fill(if learning { accent.alpha(0.12) } else { Color::TRANSPARENT })
        .hover(Color::WHITE.alpha(0.04))
        .on_press(move |s: &mut Mixer| s.learning = if s.learning == Some(learn) { None } else { Some(learn) })
        .on_secondary_press(move |s: &mut Mixer, _, _| {
            let list = if mute { &mut s.config.mute_controls } else { &mut s.config.fader_controls };
            list[index] = None;
            s.learning = None;
            s.schedule_save();
        });
    if !compact {
        chip = chip.child(label(if mute { "Mute" } else { "Fader" }, 10.0, 500.0, FAINT).a11y_hidden());
    }
    chip.child(mono(value, 9.5, color).grow(1.0).align(Align::End).truncate().a11y_hidden())
}

fn footer(s: &Mixer) -> El {
    let (status, color) = match &s.error {
        Some(error) => (error.clone(), "#FF8A8E"),
        None => (format!("{}  ·  {}", s.pipewire_status, s.midi_status), FAINT),
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
            .fill(if on { c(HAIR) } else { Color::TRANSPARENT })
            .hover(Color::WHITE.alpha(0.04))
            .on_press(move |s: &mut Mixer| {
                s.config.mute_mode = mode;
                s.schedule_save();
            })
            .child(label(name, 11.0, 500.0, if on { TEXT } else { DIM }).a11y_hidden())
    };
    let tray = s.config.close_to_tray;
    let knob = 10.0;
    row()
        .key("footer")
        .h(40.0)
        .w_full()
        .px(16.0)
        .gap(12.0)
        .center_y()
        .fill(c(PANEL))
        .child(label(status, 11.0, 400.0, color).grow(1.0).min_w(0.0).truncate())
        .child(
            row()
                .key("tray")
                .role("switch")
                .label("Close to tray")
                .help(if s.tray_available { "Closing the window keeps the mixer running in the tray." } else { "No system tray, so closing the window quits." })
                .checked(tray)
                .focusable(true)
                .h(24.0)
                .px(6.0)
                .gap(7.0)
                .center_y()
                .radius(5.0)
                .hover(Color::WHITE.alpha(0.04))
                .on_press(|s: &mut Mixer| {
                    s.config.close_to_tray = !s.config.close_to_tray;
                    s.schedule_save();
                })
                .child(
                    el().key("switch")
                        .size(24.0, 14.0)
                        .radius(7.0)
                        .fill(if tray { c(ACCENTS[0]) } else { c(HAIR) })
                        .child(el().size(knob, knob).radius(knob / 2.0).absolute(if tray { 12.0 } else { 2.0 }, 2.0).fill(Color::WHITE))
                        .a11y_hidden(),
                )
                .child(label("Close to tray", 11.0, 500.0, if s.tray_available { DIM } else { FAINT }).a11y_hidden()),
        )
        .child(
            row()
                .key("mute-mode")
                .role("radiogroup")
                .label("Mute buttons")
                .padding(2.0)
                .radius(6.0)
                .border(1.0, c(HAIR))
                .child(mode("Latch", MuteMode::Latch))
                .child(mode("Toggle", MuteMode::Toggle)),
        )
        .child(
            el().key("rescan")
                .role("button")
                .label("Rescan audio and MIDI")
                .focusable(true)
                .h(26.0)
                .px(10.0)
                .center()
                .radius(6.0)
                .border(1.0, c(HAIR))
                .hover(Color::WHITE.alpha(0.04))
                .on_press(|s: &mut Mixer| {
                    s.error = None;
                    s.rescan();
                    s.refresh_midi();
                })
                .child(label("Rescan", 11.0, 500.0, DIM).a11y_hidden()),
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
            ("midi-button".to_owned(), c(ACCENTS[0]), items)
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
                .hover(Color::WHITE.alpha(0.05))
                .on_press(move |s: &mut Mixer| {
                    match menu {
                        Menu::Assign(index) => s.assign(index, &id),
                        Menu::Midi => s.choose_port(&id),
                    }
                    s.menu = None;
                })
                .child(dot(health.map(health_color), 6.0))
                .child(label(name, 12.0, if chosen { 600.0 } else { 400.0 }, if chosen { TEXT } else { DIM }).grow(1.0).truncate().a11y_hidden())
                .child(text(if chosen { "✓" } else { "" }).font(TextStyle::new(11.0, 600.0)).color(accent).a11y_hidden()),
        );
    }
    col()
        .key("menu")
        .w(240.0)
        .max_h(340.0)
        .radius(8.0)
        .fill(c("#1F2126"))
        .border(1.0, c(HAIR))
        .shadow(8.0, 24.0, Color::BLACK.alpha(0.5))
        .clip_rounded(8.0)
        .child(list.scroll(ScrollSpec { stick_bottom: false }).shrink().min_h(0.0))
        .anchored_fit(anchor, Side::Bottom, Align::Start, 4.0)
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

    #[test]
    fn the_handle_spans_the_fader() {
        assert_eq!(handle_y(1.0, 300.0), FADER_PAD + HANDLE_H / 2.0);
        assert_eq!(handle_y(0.0, 300.0), 300.0 - FADER_PAD - HANDLE_H / 2.0);
    }
}
