//! The window: five channel strips laid out like a hardware console. Each strip has a tab naming
//! what it drives, a power button that mutes it, its level, a fader beside a column of lights, and
//! the controller bindings it learned. A bigger window zooms everything up, and the strips and
//! faders take whatever is left over.

use crate::config::{FADER_COUNT, INPUT, MIDI_DISCONNECTED, OUTPUT, STEAM_ALL};
use crate::engine::Engine;
use crate::midi::{self, Control};
use crate::mixer::{Health, Learn, Menu, Mixer, port_title};
use crate::stream::{AudioStream, Device};
use crate::updates::{RELEASES_URL, UpdateState, Updater};
use rune::{
    Align, AppSpec, Color, Cx, Drag, El, Key, KeyEvent, LineCap, Phase, ScrollSpec, Shadow, Shape, Side, Span, Spring, TextStyle, Tx, col, el,
    row, text,
};
use std::cell::Cell;
use std::time::{Duration, Instant};

/// Space above and below the fader's travel.
const FADER_PAD: f32 = 4.0;
const HANDLE_W: f32 = 24.0;
const HANDLE_H: f32 = 64.0;
const GROOVE_W: f32 = 6.0;
/// The level lights beside the fader: their size, and how many there are, spread over its travel
/// so a resize moves them rather than adding or dropping any.
const LED: f32 = 4.0;
const LIGHTS: usize = 14;
const POWER: f32 = 38.0;
const TAB_H: f32 = 44.0;
const MUTE_CELL_H: f32 = 92.0;
const LEVEL_H: f32 = 26.0;
/// The fader's height before the first layout measures it.
const FADER_GUESS: f32 = 260.0;
/// The window's default size. A bigger window zooms everything in it up from this size, the way
/// a plugin window grows, so the console fills it without stretching out of shape.
const BASE: [f32; 2] = [760.0, 600.0];
/// Wide enough for the update button's longest label, so the header doesn't shift as it changes.
const UPDATES_W: f32 = 124.0;
/// How long a strip's controller light stays on after a message.
const ACTIVITY: Duration = Duration::from_millis(140);

/// The one lit colour: warm, to keep the mixer apart from Viper Music's blues and magentas.
const ACCENT: &str = "#FF9A3C";
/// The black bars: header, tabs, levels and footer.
const BAR: &str = "#0F0F10";
/// The lines between cells.
const LINE: &str = "#1B1B1D";
const CELL_TOP: &str = "#2C2C2F";
const CELL_BOTTOM: &str = "#252527";
/// A chosen tab, or a control on a bar.
const RAISED: &str = "#38383B";
const CONTROL: &str = "#1C1C1E";
const HAIR: &str = "#2E2E31";
const LED_OFF: &str = "#47474B";
const TEXT: &str = "#E8E8EA";
const DIM: &str = "#9A9A9F";
const FAINT: &str = "#66666B";
const RED: &str = "#E5484D";

thread_local! {
    /// How much bigger than at `BASE` the window draws everything: set by `view` each frame,
    /// and read by the handlers it builds, which run between frames.
    static ZOOM: Cell<f32> = const { Cell::new(1.0) };
}

/// A size at the window's zoom. Sizes in this file are written for a `BASE` window and go
/// through here, except the 1-pixel lines between cells, which stay sharp.
fn z(size: f32) -> f32 {
    size * ZOOM.get()
}

fn font(size: f32, weight: f32) -> TextStyle {
    TextStyle::new(z(size), weight)
}

fn c(hex: &str) -> Color {
    rune::color::hex(hex).unwrap_or(Color::BLACK)
}

fn label(content: impl Into<String>, size: f32, weight: f32, color: &str) -> El {
    text(content.into()).font(font(size, weight)).color(c(color))
}

fn mono(content: impl Into<String>, size: f32, color: Color) -> El {
    text(content.into()).font(font(size, 500.0).mono()).color(color)
}

/// One line of text in parts of different sizes, weights and colours, sharing a baseline: a
/// level and its unit, say. `base` sets the line's height and font features.
fn runs(base: TextStyle, parts: &[(&str, f32, f32, &str)]) -> El {
    let mut content = String::new();
    let mut spans = Vec::new();
    for &(part, size, weight, color) in parts {
        let start = content.len();
        content.push_str(part);
        spans.push(Span::new(start, content.len()).size(z(size)).weight(weight).color(c(color)));
    }
    text(content).font(base).spans(spans)
}

fn health_color(health: Health) -> Color {
    c(match health {
        Health::Good => "#3DD68C",
        Health::Waiting => "#F5A524",
        Health::Bad => RED,
    })
}

fn dot(color: Option<Color>, size: f32) -> El {
    let size = z(size);
    el().size(size, size).radius(size / 2.0).fill(color.unwrap_or(c(HAIR))).a11y_hidden()
}

fn glow(color: Color, blur: f32) -> Shadow {
    Shadow { x: 0.0, y: 0.0, blur: z(blur), spread: 0.0, color, inset: false }
}

/// A level as the scale shows it: the cubic volume PipeWire uses, in dB.
pub(crate) fn decibels(volume: f32) -> String {
    format!("{} dB", level_number(volume))
}

fn level_number(volume: f32) -> String {
    if volume <= 0.0001 { "−∞".into() } else { format!("{:.1}", 60.0 * volume.log10()).replace('-', "−") }
}

/// Where the handle's centre sits for `volume` in a fader `height` tall.
fn handle_y(volume: f32, height: f32) -> f32 {
    z(FADER_PAD + HANDLE_H / 2.0) + (1.0 - volume) * travel(height)
}

fn travel(height: f32) -> f32 {
    (height - z(2.0 * FADER_PAD + HANDLE_H)).max(1.0)
}

pub(crate) fn spec() -> AppSpec<Mixer> {
    AppSpec {
        min_size: Some((560.0, 460.0)),
        setup: Some(|s, waker| {
            if rune::tooling::headless_run().is_none() {
                s.updates = Updater::start(waker.clone());
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
            ("update", |s| {
                demo(s);
                s.updates.show(UpdateState::Ready { version: "26.12.0".into() });
            }),
            ("disconnected", |s| {
                s.pipewire_health = Health::Bad;
                s.pipewire_status = "PipeWire unavailable · pw-dump isn't installed; install the PipeWire tools".into();
                s.midi_status = "No MIDI device · plug one in".into();
            }),
        ],
        ..AppSpec::new("Viper Mixer", (BASE[0], BASE[1]), || Mixer::new(Default::default()), view)
    }
}

fn poll(s: &mut Mixer, _: f64, tx: &mut Tx) -> bool {
    let now = Instant::now();
    let mut changed = s.drain(now) | s.updates.take_changed();
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
    // Every fader is as tall as the first.
    let fader_h = cx.layout_size("fader-0").map_or(FADER_GUESS, |[_, h]| h);
    // Zoomed by the tighter of the window's two axes, smoothly, so a resize grows everything
    // together rather than in jumps.
    let [w, h] = cx.layout_size("window").unwrap_or(BASE);
    ZOOM.set((w / BASE[0]).min(h / BASE[1]).max(1.0));
    // The gaps between strips, and between the bars, show the lines.
    let mut strips = row().key("strips").grow(1.0).w_full().min_h(0.0).gap(1.0);
    for index in 0..FADER_COUNT {
        strips = strips.child(strip(s, index, fader_h));
    }
    let mut root = col().key("window").role("window").w_full().h_full().gap(1.0).fill(c(LINE)).child(header(s));
    if let Some(banner) = update_banner(s) {
        root = root.child(banner);
    }
    root = root.child(strips).child(footer(s));
    if let Some(menu) = s.menu {
        root = root
            .child(el().key("scrim").role("none").absolute(0.0, 0.0).w_full().h_full().on_press(|s: &mut Mixer| s.menu = None))
            .child(menu_panel(s, menu));
    }
    root
}

fn logo() -> El {
    // The app icon, drawn small: a dark console with three lit knobs and three faders.
    let mut lights = row().key("lights").gap(z(2.0)).absolute(z(5.0), z(1.5));
    let mut faders = row().key("faders").gap(z(3.0)).absolute(z(5.5), z(10.0));
    for (index, top) in [6.0, 2.0, 4.0].into_iter().enumerate() {
        lights = lights.child(dot(Some(c(ACCENT)), 4.0));
        faders = faders.child(
            el().key(format!("slot{index}"))
                .size(z(3.0), z(11.0))
                .radius(z(1.5))
                .fill(c("#0B0B0C"))
                .child(el().size(z(1.0), z(11.0 - top - 3.0)).absolute(z(1.0), z(top + 3.0)).fill(c(ACCENT)))
                .child(el().size(z(5.0), z(3.0)).radius(z(1.0)).absolute(z(-1.0), z(top)).fill(c("#DCDCDF"))),
        );
    }
    el().key("logo")
        .size(z(24.0), z(24.0))
        .radius(z(6.0))
        .gradient(c("#2F2F32"), c("#222224"))
        .clip_rounded(z(6.0))
        .child(el().size(z(24.0), z(7.0)).absolute(0.0, 0.0).fill(c("#111112")))
        .child(lights)
        .child(faders)
        .a11y_hidden()
}

/// A bordered button on one of the black bars.
fn bar_button(key: &str) -> El {
    el().key(key)
        .role("button")
        .focusable(true)
        .h(z(28.0))
        .px(z(12.0))
        .center()
        .radius(z(6.0))
        .fill(c(CONTROL))
        .border(1.0, c(HAIR))
        .hover(Color::WHITE.alpha(0.05))
}

fn header(s: &Mixer) -> El {
    let health = health_color(s.pipewire_health);
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
    let update = s.updates.state();
    let checking = matches!(update, UpdateState::Checking | UpdateState::Downloading { .. });
    let updates_label = match &update {
        UpdateState::Checking => "Checking…".to_owned(),
        UpdateState::UpToDate => "Up to date".to_owned(),
        UpdateState::Downloading { percent: Some(percent), .. } => format!("Downloading {percent}%"),
        UpdateState::Downloading { percent: None, .. } => "Downloading…".to_owned(),
        _ => "Check for updates".to_owned(),
    };
    row()
        .key("header")
        .h(z(46.0))
        .w_full()
        .px(z(14.0))
        .gap(z(10.0))
        .center_y()
        .fill(c(BAR))
        .child(logo())
        .child(runs(
            font(13.5, 650.0),
            &[("Viper Mixer", 13.5, 650.0, TEXT), (concat!("   ", env!("CARGO_PKG_VERSION")), 11.0, 500.0, FAINT)],
        ))
        .child(el().grow(1.0))
        .child(
            row()
                .key("pipewire")
                .h(z(24.0))
                .px(z(10.0))
                .center()
                .radius(z(6.0))
                .fill(health.alpha(0.1))
                .border(1.0, health)
                .help(s.pipewire_status.clone())
                .child(text("PipeWire").font(font(11.0, 600.0)).color(health)),
        )
        .child(
            row()
                .key("midi-button")
                .role("button")
                .label("MIDI device")
                .help(s.midi_status.clone())
                .focusable(true)
                .w(z(210.0))
                .shrink()
                .min_w(z(120.0))
                .h(z(28.0))
                .px(z(10.0))
                .gap(z(8.0))
                .center_y()
                .radius(z(6.0))
                .fill(c(CONTROL))
                .border(1.0, if open { c(ACCENT) } else { c(HAIR) })
                .hover(Color::WHITE.alpha(0.05))
                .on_press(|s: &mut Mixer| s.menu = if s.menu == Some(Menu::Midi) { None } else { Some(Menu::Midi) })
                .child(dot(Some(health_color(s.midi_health())), 6.0))
                .child(runs(font(12.0, 500.0), &[("MIDI   ", 10.0, 600.0, FAINT), (&port, 12.0, 500.0, TEXT)]).grow(1.0).truncate())
                .child(label("▾", 10.0, 500.0, FAINT)),
        )
        .child(
            bar_button("updates")
                .w(z(UPDATES_W))
                // Its label and highlight change at once, without sliding or fading.
                .instant_changes()
                .motion(Spring::NONE)
                .label(format!("Check for updates to Viper Mixer {}", env!("CARGO_PKG_VERSION")))
                .a11y_value(updates_label.clone())
                .focusable(!checking)
                .hover(Color::WHITE.alpha(if checking { 0.0 } else { 0.05 }))
                .on_press(move |s: &mut Mixer| {
                    if !checking {
                        s.updates.check_now();
                    }
                })
                .child(label(updates_label, 11.0, 500.0, TEXT).a11y_hidden()),
        )
}

fn strip(s: &Mixer, index: usize, fader_h: f32) -> El {
    col()
        .key(format!("strip-{index}"))
        .w(z(0.0))
        .grow(1.0)
        .min_w(z(84.0))
        .gap(1.0)
        .child(name_tab(s, index))
        .child(
            col()
                .key("mute-cell")
                .h(z(MUTE_CELL_H))
                .w_full()
                .gradient(c(CELL_TOP), c(CELL_BOTTOM))
                .center()
                .px(z(8.0))
                .gap(z(10.0))
                .child(power_button(s, index))
                .child(binding_chip(s, index, true)),
        )
        .child(
            row()
                .key("level")
                .h(z(LEVEL_H))
                .w_full()
                .fill(c(BAR))
                .justify_center()
                .items_center()
                .gap(z(3.0))
                .child(runs(
                    font(12.0, 600.0).features("'tnum' 1"),
                    &[(&level_number(s.config.fader_volumes[index]), 12.0, 600.0, TEXT), (" dB", 9.5, 500.0, FAINT)],
                )),
        )
        .child(
            col()
                .key("fader-cell")
                .w_full()
                .grow(1.0)
                .min_h(0.0)
                .gradient(c(CELL_TOP), c(CELL_BOTTOM))
                .px(z(8.0))
                .pt(z(10.0))
                .pb(z(8.0))
                .gap(z(6.0))
                .child(fader(s, index, fader_h))
                .child(binding_chip(s, index, false)),
        )
}

/// The strip's tab: what it drives, and a press picks something else.
fn name_tab(s: &Mixer, index: usize) -> El {
    let health = if s.is_live(index) {
        Some(Health::Good)
    } else if s.is_present(index) {
        Some(Health::Waiting)
    } else {
        None
    };
    let open = s.menu == Some(Menu::Assign(index));
    el().key(format!("tab-{index}")).h(z(TAB_H)).w_full().px(z(6.0)).center().fill(c(BAR)).child(
        col()
            .key(format!("assign-{index}"))
            .role("button")
            .label(format!("Strip {} assignment", index + 1))
            .focusable(true)
            .w_full()
            .px(z(6.0))
            .py(z(4.0))
            .gap(1.0)
            .items_center()
            .radius(z(6.0))
            .fill(if open { c(RAISED) } else { Color::TRANSPARENT })
            .hover(Color::WHITE.alpha(0.06))
            .on_press(move |s: &mut Mixer| {
                s.menu = if s.menu == Some(Menu::Assign(index)) { None } else { Some(Menu::Assign(index)) };
            })
            .child(
                row()
                    .w_full()
                    .justify_center()
                    .gap(z(5.0))
                    .center_y()
                    .child(dot(health.map(health_color), 5.0))
                    .child(label(s.strip_title(index), 12.0, 600.0, TEXT).shrink().truncate())
                    .child(label("▾", 9.0, 500.0, FAINT)),
            )
            .child(label(s.strip_detail(index), 10.0, 400.0, FAINT).w_full().align(Align::Center).truncate()),
    )
}

/// A power symbol: a ring open at the top, and a bar through the gap.
fn power_icon(color: Color) -> El {
    let (x, y, r) = (z(8.0), z(8.5), z(6.0));
    let arc: Vec<[f32; 2]> = (0..=24)
        .map(|step| {
            let angle = (40.0 + 280.0 * step as f32 / 24.0f32).to_radians();
            [x + r * angle.sin(), y - r * angle.cos()]
        })
        .collect();
    el().size(z(16.0), z(16.0))
        .shape(Shape::new().stroke(arc, z(1.8), color).line([x, z(1.5)], [x, z(8.0)], z(1.8), LineCap::Round, color))
        .a11y_hidden()
}

/// The strip's mute, as a power button: dark while the strip plays, glowing red while it's muted.
fn power_button(s: &Mixer, index: usize) -> El {
    let muted = s.muted[index];
    let red = c(RED);
    let button = el()
        .key(format!("mute-{index}"))
        .role("switch")
        .label(format!("Mute strip {}", index + 1))
        .help(if muted { "Muted. Press to unmute." } else { "Playing. Press to mute." })
        .checked(muted)
        .focusable(true)
        .size(z(POWER), z(POWER))
        .radius(z(POWER / 2.0))
        .center()
        .gradient(c("#48484C"), c("#2A2A2D"))
        .border(z(2.0), if muted { red } else { c("#161618") })
        .shadow(z(2.0), z(5.0), Color::BLACK.alpha(0.6))
        .hover(Color::WHITE.alpha(0.05))
        .on_press(move |s: &mut Mixer| s.set_muted(index, !s.muted[index]))
        .child(power_icon(if muted { red } else { c(DIM) }));
    if muted { button.box_shadow(glow(red.alpha(0.55), 12.0)) } else { button }
}

/// The lights beside a fader, lit from the bottom up to its level; a muted strip's are grey. The
/// bottom one marks silence and stays dark.
fn level_lights(volume: f32, height: f32, muted: bool) -> El {
    let (top, bottom, at) = (handle_y(1.0, height), handle_y(0.0, height), handle_y(volume, height));
    let lit = if muted { c(DIM) } else { c(ACCENT) };
    let mut lights = el().key("lights").w(z(LED)).h_full().a11y_hidden();
    for step in 0..LIGHTS {
        let y = top + (bottom - top) * step as f32 / (LIGHTS - 1) as f32;
        let floor = step == LIGHTS - 1;
        let on = !floor && volume > 0.0 && y >= at - 0.5;
        let off = if floor { c("#0A0A0B") } else { c(LED_OFF) };
        let mut light = el().size(z(LED), z(LED)).radius(z(LED / 2.0)).absolute(0.0, y - z(LED / 2.0)).fill(if on { lit } else { off });
        if on && !muted {
            light = light.box_shadow(glow(lit.alpha(0.7), 6.0));
        }
        lights = lights.child(light);
    }
    lights
}

/// A fader cap's rows from top to bottom, taken pixel by pixel from the reference console's
/// 48-pixel cap: a lit top face, fine ribs either side of a white line across the middle, and a
/// black bottom face. They are stretched evenly to `HANDLE_H`.
const CAP_ROWS: [&str; 48] = [
    "#444444", "#969696", "#828282", "#888888", "#868686", "#848484", "#999999", "#292929",
    "#000000", "#292929", "#5E5E5E", "#0C0C0C", "#222222", "#161616", "#6B6B6B", "#5F5F5F",
    "#1A1A1A", "#2F2F2F", "#3A3A3A", "#8E8E8E", "#5B5B5B", "#2B2B2B", "#636363", "#E1E1E1",
    "#DEDEDE", "#C5C5C5", "#484848", "#5E5E5E", "#1C1C1C", "#131313", "#6C6C6C", "#636363",
    "#737373", "#101010", "#505050", "#7D7D7D", "#7D7D7D", "#737373", "#2D2D2D", "#828282",
    "#9B9B9B", "#363636", "#080808", "#111111", "#111111", "#111111", "#111111", "#111111",
];

/// A fader cap: a flat block of grey ribs with a white line across its middle.
fn cap(y: f32) -> El {
    let mut cap = el()
        .key("handle")
        .size(z(HANDLE_W), z(HANDLE_H))
        .absolute(0.0, y - z(HANDLE_H / 2.0))
        .box_shadow(Shadow { x: 0.0, y: z(1.0), blur: z(3.0), spread: z(1.0), color: Color::BLACK.alpha(0.6), inset: false });
    let row_h = z(HANDLE_H) / CAP_ROWS.len() as f32;
    for (row, color) in CAP_ROWS.iter().enumerate() {
        cap = cap.child(el().size(z(HANDLE_W), row_h).absolute(0.0, row as f32 * row_h).fill(c(color)));
    }
    cap
}

fn fader(s: &Mixer, index: usize, height: f32) -> El {
    let volume = s.config.fader_volumes[index];
    let muted = s.muted[index];
    let lit = s.touched_at[index].is_some_and(|at| at.elapsed() < ACTIVITY);
    let accent = c(ACCENT);
    let y = handle_y(volume, height);
    let (top, bottom) = (handle_y(1.0, height), handle_y(0.0, height));
    let groove_x = z((HANDLE_W - GROOVE_W) / 2.0);
    let track = el()
        .key("track")
        .w(z(HANDLE_W))
        .h_full()
        .child(
            el().key("groove")
                .size(z(GROOVE_W), bottom - top + z(GROOVE_W))
                .absolute(groove_x, top - z(GROOVE_W / 2.0))
                .radius(z(GROOVE_W / 2.0))
                .fill(c("#0B0B0C"))
                .border_gradient(1.0, Color::BLACK, c("#3A3A3D")),
        )
        .child(el().key("fill").size(z(2.0), (bottom - y).max(0.0)).absolute(z((HANDLE_W - 2.0) / 2.0), y).radius(z(1.0)).fill(if muted { c(DIM) } else { accent }))
        .child(cap(y))
        .a11y_hidden();
    // The thin line on the right, in two segments: both light while the controller moves the
    // strip, and the lower one glows red while it's muted.
    let split = top + (bottom - top) * 0.42;
    let segment = |from: f32, to: f32, color: Option<Color>| {
        let line = el().size(z(2.0), (to - from).max(0.0)).absolute(0.0, from).radius(z(1.0)).fill(color.unwrap_or(c(LED_OFF)));
        match color {
            Some(color) => line.box_shadow(glow(color.alpha(0.7), 6.0)),
            None => line,
        }
    };
    let upper = segment(top, split - z(3.0), lit.then_some(accent));
    let lower = segment(split + z(3.0), bottom, if muted { Some(c(RED)) } else { lit.then_some(accent) });
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
        .gap(z(14.0))
        .on_drag(move |s: &mut Mixer, d: &mut Drag, tx| match d.phase {
            Phase::Down => {
                let at = handle_y(s.config.fader_volumes[index], d.h);
                if d.clicks == 2 {
                    // A double press puts the fader back at unity.
                    s.set_volume(index, 1.0);
                } else if (d.y - at).abs() > z(HANDLE_H / 2.0) {
                    // Pressed beside the handle: it jumps there and drags from it.
                    s.set_volume(index, 1.0 - (d.y - z(FADER_PAD + HANDLE_H / 2.0)) / travel(d.h));
                }
                d.data = s.config.fader_volumes[index];
            }
            Phase::Move => {
                s.set_volume(index, d.data - (d.y - d.start_y) / travel(d.h));
                tx.instant();
            }
            Phase::Up => {}
        })
        .child(level_lights(volume, height, muted))
        .child(track)
        .child(el().key("signal").w(z(2.0)).h_full().child(upper).child(lower).a11y_hidden())
}

/// The control a strip's mute or fader learned, under the power button or the fader. A press
/// learns a new one; a right-click unbinds it.
fn binding_chip(s: &Mixer, index: usize, mute: bool) -> El {
    let learn = if mute { Learn::Mute(index) } else { Learn::Fader(index) };
    let learning = s.learning == Some(learn);
    let bound = if mute { s.config.mute_controls[index] } else { s.config.fader_controls[index] };
    let accent = c(ACCENT);
    let (value, color) = if learning {
        ("Listening…".to_owned(), accent)
    } else {
        match bound {
            Some(control) => (control.label(), c("#D4D4D7")),
            None => ("Learn".to_owned(), c(FAINT)),
        }
    };
    let kind = if mute { "mute" } else { "fader" };
    el().key(format!("{kind}-learn-{index}"))
        .role("button")
        .label(format!("Learn the {kind} control for strip {}", index + 1))
        .help(format!(
            "The {kind} control. Press, then move a fader or press a button on the controller. Right-click to unbind."
        ))
        .focusable(true)
        .w_full()
        .h(z(20.0))
        .px(z(6.0))
        .center()
        .radius(z(4.0))
        .fill(if learning { accent.alpha(0.14) } else { Color::TRANSPARENT })
        .hover(Color::WHITE.alpha(0.05))
        .on_press(move |s: &mut Mixer| s.learning = if s.learning == Some(learn) { None } else { Some(learn) })
        .on_secondary_press(move |s: &mut Mixer, _, _| {
            let list = if mute { &mut s.config.mute_controls } else { &mut s.config.fader_controls };
            list[index] = None;
            s.learning = None;
            s.schedule_save();
        })
        .child(mono(value, 9.5, color).truncate().a11y_hidden())
}

fn footer(s: &Mixer) -> El {
    let (status, color) = match (&s.error, s.updates.state()) {
        (Some(error), _) => (error.clone(), "#FF8A8E"),
        (None, UpdateState::Failed { message }) => (message, "#FF8A8E"),
        _ => (format!("{}  ·  {}", s.pipewire_status, s.midi_status), FAINT),
    };
    let tray = s.config.close_to_tray;
    let knob = z(10.0);
    row()
        .key("footer")
        .h(z(36.0))
        .w_full()
        .px(z(14.0))
        .gap(z(12.0))
        .center_y()
        .fill(c(BAR))
        .child(label(status, 11.0, 400.0, color).grow(1.0).min_w(z(0.0)).truncate())
        .child(
            row()
                .key("tray")
                .role("switch")
                .label("Close to tray")
                .help(if s.tray_available { "Closing the window keeps the mixer running in the tray." } else { "No system tray, so closing the window quits." })
                .checked(tray)
                .focusable(true)
                .h(z(24.0))
                .px(z(6.0))
                .gap(z(7.0))
                .center_y()
                .radius(z(5.0))
                .hover(Color::WHITE.alpha(0.05))
                .on_press(|s: &mut Mixer| {
                    s.config.close_to_tray = !s.config.close_to_tray;
                    s.schedule_save();
                })
                .child(
                    el().key("switch")
                        .size(z(24.0), z(14.0))
                        .radius(z(7.0))
                        .fill(if tray { c(ACCENT) } else { c(RAISED) })
                        .child(el().size(knob, knob).radius(knob / 2.0).absolute(z(if tray { 12.0 } else { 2.0 }), z(2.0)).fill(Color::WHITE))
                        .a11y_hidden(),
                )
                .child(label("Close to tray", 11.0, 500.0, if s.tray_available { DIM } else { FAINT }).a11y_hidden()),
        )
        .child(
            bar_button("rescan")
                .label("Rescan audio and MIDI")
                .h(z(24.0))
                .px(z(10.0))
                .on_press(|s: &mut Mixer| {
                    s.error = None;
                    s.rescan();
                    s.refresh_midi();
                })
                .child(label("Rescan", 11.0, 500.0, DIM).a11y_hidden()),
        )
}

/// A strip of the window under the header once an update is installed, or, for a copy that
/// can't install it, released.
fn update_banner(s: &Mixer) -> Option<El> {
    let (version, ready) = match s.updates.state() {
        UpdateState::Ready { version } => (version, true),
        UpdateState::Available { version } => (version, false),
        _ => return None,
    };
    if s.update_dismissed.as_deref() == Some(version.as_str()) {
        return None;
    }
    let accent = c(ACCENT);
    let message = if ready {
        format!("Viper Mixer {version} is installed. Restart to use it.")
    } else {
        format!("Viper Mixer {version} is out.")
    };
    let dismissed = version.clone();
    Some(
        row()
            .key("update")
            .role("status")
            .h(z(38.0))
            .w_full()
            .px(z(14.0))
            .gap(z(10.0))
            .center_y()
            .fill(c("#1E1710"))
            .child(dot(Some(accent), 6.0).box_shadow(glow(accent.alpha(0.7), 6.0)))
            .child(label(message, 12.0, 500.0, TEXT).grow(1.0).min_w(z(0.0)).truncate())
            .child(
                el().key("update-action")
                    .role("button")
                    .label(if ready { "Restart Viper Mixer into the update" } else { "Open the release page" })
                    .focusable(true)
                    .h(z(26.0))
                    .px(z(12.0))
                    .center()
                    .radius(z(6.0))
                    .fill(accent)
                    .on_press_tx(move |s: &mut Mixer, tx| {
                        if !ready {
                            if let Err(error) = tx.open_url(RELEASES_URL) {
                                s.error = Some(format!("Couldn't open the release page: {error}"));
                            }
                        } else if s.updates.restart() {
                            s.menu = None;
                            s.learning = None;
                            s.save_now();
                            tx.quit();
                        }
                    })
                    .child(label(if ready { "Restart" } else { "Download" }, 11.0, 600.0, "#111112").a11y_hidden()),
            )
            .child(
                el().key("update-dismiss")
                    .role("button")
                    .label("Dismiss")
                    .focusable(true)
                    .size(z(26.0), z(26.0))
                    .center()
                    .radius(z(6.0))
                    .hover(Color::WHITE.alpha(0.06))
                    .on_press(move |s: &mut Mixer| s.update_dismissed = Some(dismissed.clone()))
                    .child(label("×", 14.0, 500.0, DIM).a11y_hidden()),
            ),
    )
}

/// A menu row: its id, label, light, and whether it's the current choice.
type MenuItem = (String, String, Option<Health>, bool);

fn menu_panel(s: &Mixer, menu: Menu) -> El {
    let (anchor, items): (String, Vec<MenuItem>) = match menu {
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
            (format!("assign-{index}"), items)
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
            ("midi-button".to_owned(), items)
        }
    };
    let mut list = col().key("items").role("menu").gap(1.0).padding(z(4.0));
    for (position, (id, name, health, chosen)) in items.into_iter().enumerate() {
        let id = std::rc::Rc::new(id);
        list = list.child(
            row()
                .key(format!("item-{position}"))
                .role("menuitem")
                .label(name.clone())
                .checked(chosen)
                .focusable(true)
                .h(z(28.0))
                .px(z(8.0))
                .gap(z(8.0))
                .center_y()
                .radius(z(5.0))
                .hover(Color::WHITE.alpha(0.06))
                .on_press(move |s: &mut Mixer| {
                    match menu {
                        Menu::Assign(index) => s.assign(index, &id),
                        Menu::Midi => s.choose_port(&id),
                    }
                    s.menu = None;
                })
                .child(dot(health.map(health_color), 6.0))
                .child(label(name, 12.0, if chosen { 600.0 } else { 400.0 }, if chosen { TEXT } else { DIM }).grow(1.0).truncate().a11y_hidden())
                .child(text(if chosen { "✓" } else { "" }).font(font(11.0, 600.0)).color(c(ACCENT)).a11y_hidden()),
        );
    }
    col()
        .key("menu")
        .w(z(240.0))
        .max_h(z(340.0))
        .radius(z(8.0))
        .fill(c("#222224"))
        .border(1.0, c("#3A3A3D"))
        .shadow(z(8.0), z(24.0), Color::BLACK.alpha(0.6))
        .clip_rounded(z(8.0))
        .child(list.scroll(ScrollSpec { stick_bottom: false }).shrink().min_h(0.0))
        .anchored_fit(anchor, Side::Bottom, Align::Start, z(4.0))
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
    }

    #[test]
    fn the_handle_spans_the_fader() {
        assert_eq!(handle_y(1.0, 300.0), FADER_PAD + HANDLE_H / 2.0);
        assert_eq!(handle_y(0.0, 300.0), 300.0 - FADER_PAD - HANDLE_H / 2.0);
    }

    #[test]
    fn the_cap_line_crosses_the_middle() {
        let brightest = (0..CAP_ROWS.len()).max_by_key(|&row| CAP_ROWS[row]).unwrap();
        assert!((brightest as f32 + 0.5 - CAP_ROWS.len() as f32 / 2.0).abs() <= 1.0);
    }
}
