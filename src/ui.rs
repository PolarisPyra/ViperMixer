//! The window: five channel strips laid out like a hardware console. Each strip has a tab naming
//! what it drives, a power button that mutes it, its level, a fader beside a column of lights, and
//! the controller bindings it learned. A bigger window zooms everything up, and the strips and
//! faders take whatever is left over.

use crate::config::{Config, FADER_COUNT, INPUT, MIDI_DISCONNECTED, OUTPUT, STEAM_ALL};
use crate::midi::{self, Control};
use crate::mixer::{Health, Learn, Menu, Mixer, port_title};
use crate::signal::Signal;
use crate::stream::{AudioStream, Device};
use crate::updates::{RELEASES_URL, UpdateState};
use futures::StreamExt;
use gpui::prelude::FluentBuilder;
use gpui::{
    AnyView, App, AppContext, Application, Bounds, BoxShadow, ClickEvent, Context, Corners, FocusHandle, FontFeatures, FontWeight, Hsla, InteractiveElement, IntoElement,
    KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent, ParentElement, PathBuilder, Pixels, Render, SharedString, StatefulInteractiveElement,
    Styled, TitlebarOptions, Window, WindowBounds, WindowHandle, WindowOptions, anchored, canvas, deferred, div, fill, linear_color_stop,
    linear_gradient, point, px, rgb, size,
};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Space above and below the fader's travel.
const FADER_PAD: f32 = 4.0;
/// The fader cap, at the reference console's proportions: one of `CAP_ROWS` per point.
const HANDLE_W: f32 = 21.0;
const HANDLE_H: f32 = 47.0;
/// The slot the cap runs in: a lit rim either side of a dark channel, inside a dark edge.
const GROOVE_W: f32 = 9.0;
/// The level lights beside the fader: their size, and how many there are, spread over its travel
/// so a resize moves them rather than adding or dropping any.
const LED: f32 = 5.0;
const LIGHTS: usize = 14;
const POWER: f32 = 38.0;
const TAB_H: f32 = 44.0;
const MUTE_CELL_H: f32 = 92.0;
const LEVEL_H: f32 = 26.0;
/// The window's default size. A bigger window zooms everything in it up from this size, the way
/// a plugin window grows, so the console fills it without stretching out of shape.
const BASE: [f32; 2] = [760.0, 600.0];
const MIN: [f32; 2] = [560.0, 460.0];
/// Wide enough for the update button's longest label, so the header doesn't shift as it changes.
const UPDATES_W: f32 = 124.0;
/// How long a strip's controller light stays on after a message.
const ACTIVITY: Duration = Duration::from_millis(140);

/// The one lit colour: warm, to keep the mixer apart from Viper Music's blues and magentas.
const ACCENT: u32 = 0xFF9A3C;
/// The black bars: header, tabs, levels and footer.
const BAR: u32 = 0x0F0F10;
/// The lines between cells.
const LINE: u32 = 0x1B1B1D;
/// The grey cells, which fade gently from top to bottom, and the fader cells, which stay flat.
const CELL_TOP: u32 = 0x2C2C2F;
const CELL_BOTTOM: u32 = 0x252527;
const CELL: u32 = 0x29292B;
/// A chosen tab, or a control on a bar.
const RAISED: u32 = 0x38383B;
const CONTROL: u32 = 0x1C1C1E;
const HAIR: u32 = 0x2E2E31;
const LED_OFF: u32 = 0x47474B;
const TEXT: u32 = 0xE8E8EA;
const DIM: u32 = 0x9A9A9F;
const FAINT: u32 = 0x66666B;
const RED: u32 = 0xE5484D;

thread_local! {
    /// How much bigger than at `BASE` the window draws everything: set by `render` each frame,
    /// and read by the handlers it builds, which run between frames.
    static ZOOM: Cell<f32> = const { Cell::new(1.0) };
}

/// A size at the window's zoom. Sizes in this file are written for a `BASE` window and go
/// through here, except the 1-pixel lines between cells, which stay sharp.
fn z(size: f32) -> f32 {
    size * ZOOM.get()
}

fn zp(size: f32) -> Pixels {
    px(z(size))
}

fn c(hex: u32) -> Hsla {
    rgb(hex).into()
}

/// Text in one size, weight and colour.
fn label(content: impl Into<SharedString>, size: f32, weight: f32, color: Hsla) -> gpui::Div {
    div().text_size(zp(size)).font_weight(FontWeight(weight)).text_color(color).child(content.into())
}

/// Text whose digits all take the same width, so a changing level doesn't shuffle sideways.
fn tabular(mut text: gpui::Div) -> gpui::Div {
    text.text_style().get_or_insert_with(Default::default).font_features = Some(FontFeatures(Arc::new(vec![("tnum".into(), 1)])));
    text
}

fn health_color(health: Health) -> Hsla {
    c(match health {
        Health::Good => 0x3DD68C,
        Health::Waiting => 0xF5A524,
        Health::Bad => RED,
    })
}

fn dot(color: Option<Hsla>, size: f32) -> gpui::Div {
    div().flex_none().size(zp(size)).rounded_full().bg(color.unwrap_or(c(HAIR)))
}

fn glow(color: Hsla, blur: f32) -> BoxShadow {
    BoxShadow { color, offset: point(px(0.0), px(0.0)), blur_radius: zp(blur), spread_radius: px(0.0) }
}

/// A level as the scale shows it, without its unit: the cubic volume PipeWire uses, in dB.
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

/// How the window closed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Exit {
    /// To the tray: the mixer keeps running without it.
    Hide,
    Quit,
}

/// Opens the window and runs it until it closes. A `demo` window never saves.
pub(crate) fn run(mixer: Rc<RefCell<Mixer>>, signal: Signal, demo: bool) -> Exit {
    let exit = Rc::new(Cell::new(Exit::Quit));
    let outcome = exit.clone();
    let window_signal = signal.clone();
    Application::new().run(move |cx: &mut App| {
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds::centered(None, size(px(BASE[0]), px(BASE[1])), cx))),
            titlebar: Some(TitlebarOptions { title: Some("Viper Mixer".into()), ..Default::default() }),
            app_id: Some("viper-mixer".into()),
            window_min_size: Some(size(px(MIN[0]), px(MIN[1]))),
            ..Default::default()
        };
        let messages = window_signal.open_window();
        let opened = cx.open_window(options, |window, cx| {
            cx.new(|cx| {
                let focus = cx.focus_handle();
                window.focus(&focus);
                Console { mixer, signal: window_signal.clone(), exit: exit.clone(), demo, focus, drag: None, faders: Rc::default(), anchors: Rc::default() }
            })
        });
        let Ok(handle) = opened else {
            eprintln!("Couldn't open the window.");
            cx.quit();
            return;
        };
        let _ = handle.update(cx, |console, window, cx| {
            let entity = cx.entity().downgrade();
            window.on_window_should_close(cx, move |_, cx| {
                entity.update(cx, |console, _| console.closing()).ok();
                true
            });
            console.listen(handle, messages, cx);
        });
    });
    signal.close_window();
    outcome.get()
}

/// Keeps the mixer following the controller while the window is closed to the tray. Returns true
/// once something asks to show the window, false to quit.
pub(crate) fn wait_hidden(mixer: &mut Mixer, signal: &Signal) -> bool {
    let mut next = Duration::ZERO;
    loop {
        let pending = signal.wait(next);
        if pending.quit {
            return false;
        }
        if pending.show {
            return true;
        }
        let now = Instant::now();
        mixer.drain(now);
        mixer.updates.take_changed();
        next = mixer.tick(now);
    }
}

pub(crate) struct Console {
    mixer: Rc<RefCell<Mixer>>,
    signal: Signal,
    exit: Rc<Cell<Exit>>,
    demo: bool,
    focus: FocusHandle,
    drag: Option<FaderDrag>,
    /// Each fader's bounds, from the last frame, for turning the pointer into a level.
    faders: Rc<RefCell<[Bounds<Pixels>; FADER_COUNT]>>,
    /// Where the menu buttons are, from the last frame, for hanging their menus under them.
    anchors: Rc<RefCell<HashMap<String, Bounds<Pixels>>>>,
}

/// A fader being dragged: where the pointer went down and the level it had then.
#[derive(Clone, Copy)]
struct FaderDrag {
    index: usize,
    start_y: f32,
    start: f32,
}

impl Console {
    /// Reads what the workers sent each time one wakes the window, and runs the mixer's periodic
    /// work when it's due.
    fn listen(&self, handle: WindowHandle<Console>, mut messages: futures::channel::mpsc::UnboundedReceiver<()>, cx: &mut Context<Self>) {
        cx.spawn(async move |_, cx| {
            loop {
                let Ok(next) = handle.update(cx, |console, window, cx| console.poll(window, cx)) else { break };
                let timer = cx.background_executor().timer(next);
                if let futures::future::Either::Left((None, _)) = futures::future::select(messages.next(), timer).await {
                    break;
                }
            }
        })
        .detach();
    }

    fn poll(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Duration {
        let pending = self.signal.take();
        if pending.quit {
            self.closing();
            self.exit.set(Exit::Quit);
            cx.quit();
        }
        if pending.show {
            window.activate_window();
        }
        let now = Instant::now();
        let mut s = self.mixer.borrow_mut();
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
        if changed {
            cx.notify();
        }
        next
    }

    /// The window is closing: to the tray, unless that's off or there's no tray.
    fn closing(&mut self) {
        let mut s = self.mixer.borrow_mut();
        s.menu = None;
        s.learning = None;
        if !self.demo {
            s.save_now();
        }
        let hide = s.config.close_to_tray && s.tray_available && !self.demo;
        self.exit.set(if hide { Exit::Hide } else { Exit::Quit });
    }

    fn on_key(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let mut s = self.mixer.borrow_mut();
        if event.keystroke.key == "escape" && (s.learning.is_some() || s.menu.is_some()) {
            s.learning = None;
            s.menu = None;
            cx.notify();
        }
    }

    fn fader_down(&mut self, index: usize, event: &MouseDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let bounds = self.faders.borrow()[index];
        let height = f32::from(bounds.size.height);
        let y = f32::from(event.position.y - bounds.origin.y);
        let mut s = self.mixer.borrow_mut();
        let at = handle_y(s.config.fader_volumes[index], height);
        if event.click_count == 2 {
            // A double press puts the fader back at unity.
            s.set_volume(index, 1.0);
        } else if (y - at).abs() > z(HANDLE_H / 2.0) {
            // Pressed beside the handle: it jumps there and drags from it.
            s.set_volume(index, 1.0 - (y - z(FADER_PAD + HANDLE_H / 2.0)) / travel(height));
        }
        self.drag = Some(FaderDrag { index, start_y: y, start: s.config.fader_volumes[index] });
        cx.notify();
    }

    fn pointer_moved(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        let Some(drag) = self.drag else { return };
        if event.pressed_button != Some(MouseButton::Left) {
            self.drag = None;
            return;
        }
        let bounds = self.faders.borrow()[drag.index];
        let y = f32::from(event.position.y - bounds.origin.y);
        self.mixer.borrow_mut().set_volume(drag.index, drag.start - (y - drag.start_y) / travel(f32::from(bounds.size.height)));
        cx.notify();
    }
}

/// A click handler that changes the mixer and redraws.
fn act(cx: &mut Context<Console>, change: impl Fn(&mut Mixer) + 'static) -> impl Fn(&ClickEvent, &mut Window, &mut App) + 'static {
    cx.listener(move |console, _, _, cx| {
        change(&mut console.mixer.borrow_mut());
        cx.notify();
    })
}

/// Records where an element is each frame, under `key`, for a menu to hang from.
fn anchor(anchors: &Rc<RefCell<HashMap<String, Bounds<Pixels>>>>, key: String) -> impl IntoElement {
    let anchors = anchors.clone();
    canvas(move |bounds, _, _| anchors.borrow_mut().insert(key, bounds), |_, _, _, _| {}).absolute().size_full()
}

/// A tooltip: one line of help on a dark plate.
struct Tip(SharedString);

impl Render for Tip {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .px(zp(8.0))
            .py(zp(5.0))
            .rounded(zp(5.0))
            .bg(c(0x222224))
            .border_1()
            .border_color(c(0x3A3A3D))
            .text_size(zp(11.0))
            .text_color(c(TEXT))
            .child(self.0.clone())
    }
}

fn tip(text: impl Into<SharedString>) -> impl Fn(&mut Window, &mut App) -> AnyView + 'static {
    let text = text.into();
    move |_, cx| cx.new(|_| Tip(text.clone())).into()
}

impl Render for Console {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Zoomed by the tighter of the window's two axes, smoothly, so a resize grows everything
        // together rather than in jumps.
        let viewport = window.viewport_size();
        ZOOM.set((f32::from(viewport.width) / BASE[0]).min(f32::from(viewport.height) / BASE[1]).max(1.0));
        let mixer = self.mixer.clone();
        let s = mixer.borrow();
        // The gaps between strips, and between the bars, show the lines.
        let mut strips = div().flex().flex_grow().w_full().min_h(px(0.0)).gap(px(1.0));
        for index in 0..FADER_COUNT {
            strips = strips.child(self.strip(&s, index, cx));
        }
        let mut root = div()
            .id("window")
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Console::on_key))
            .on_mouse_move(cx.listener(Console::pointer_moved))
            .on_mouse_up(MouseButton::Left, cx.listener(|console, _, _, _| console.drag = None))
            .relative()
            .flex()
            .flex_col()
            .size_full()
            .gap(px(1.0))
            .bg(c(LINE))
            .text_color(c(TEXT))
            .child(self.header(&s, cx));
        if let Some(banner) = update_banner(&s, cx) {
            root = root.child(banner);
        }
        root = root.child(strips).child(footer(&s, cx));
        if let Some(menu) = s.menu {
            root = root
                .child(deferred(
                    div().absolute().top_0().left_0().size_full().occlude().on_mouse_down(MouseButton::Left, cx.listener(|console, _, _, cx| {
                        console.mixer.borrow_mut().menu = None;
                        cx.notify();
                    })),
                ))
                .child(deferred(menu_panel(&s, menu, &self.anchors, cx)).with_priority(1));
        }
        root
    }
}

impl Console {
    fn header(&self, s: &Mixer, cx: &mut Context<Self>) -> impl IntoElement {
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
        let mut updates = bar_button("updates").w(zp(UPDATES_W)).child(label(updates_label, 11.0, 500.0, c(TEXT)));
        if !checking {
            updates = updates.hover(|style| style.bg(gpui::white().alpha(0.05))).on_click(act(cx, |s| s.updates.check_now()));
        }
        div()
            .flex()
            .flex_none()
            .h(zp(46.0))
            .w_full()
            .px(zp(14.0))
            .gap(zp(10.0))
            .items_center()
            .bg(c(BAR))
            .child(
                div()
                    .flex()
                    .items_baseline()
                    .gap(zp(10.0))
                    .child(label("Viper Mixer", 13.5, 650.0, c(TEXT)))
                    .child(label(env!("CARGO_PKG_VERSION"), 11.0, 500.0, c(FAINT))),
            )
            .child(div().flex_grow())
            .child(
                div()
                    .id("pipewire")
                    .flex()
                    .flex_none()
                    .h(zp(24.0))
                    .px(zp(10.0))
                    .items_center()
                    .rounded(zp(6.0))
                    .bg(health.alpha(0.1))
                    .border_1()
                    .border_color(health)
                    .tooltip(tip(s.pipewire_status.clone()))
                    .child(label("PipeWire", 11.0, 600.0, health)),
            )
            .child(
                div()
                    .id("midi-button")
                    .relative()
                    .flex()
                    .flex_shrink()
                    .w(zp(210.0))
                    .min_w(zp(120.0))
                    .h(zp(28.0))
                    .px(zp(10.0))
                    .gap(zp(8.0))
                    .items_center()
                    .rounded(zp(6.0))
                    .bg(c(CONTROL))
                    .border_1()
                    .border_color(if open { c(ACCENT) } else { c(HAIR) })
                    .hover(|style| style.bg(c(0x252527)))
                    .tooltip(tip(s.midi_status.clone()))
                    .on_click(act(cx, |s| s.menu = if s.menu == Some(Menu::Midi) { None } else { Some(Menu::Midi) }))
                    .child(anchor(&self.anchors, "midi-button".into()))
                    .child(dot(Some(health_color(s.midi_health())), 6.0))
                    .child(
                        div()
                            .flex()
                            .flex_grow()
                            .min_w(px(0.0))
                            .items_baseline()
                            .gap(zp(8.0))
                            .child(label("MIDI", 10.0, 600.0, c(FAINT)).flex_none())
                            .child(label(port, 12.0, 500.0, c(TEXT)).min_w(px(0.0)).truncate()),
                    )
                    .child(label("▾", 10.0, 500.0, c(FAINT))),
            )
            .child(updates)
    }

    fn strip(&self, s: &Mixer, index: usize, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w(zp(84.0))
            .gap(px(1.0))
            .child(self.name_tab(s, index, cx))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_none()
                    .h(zp(MUTE_CELL_H))
                    .w_full()
                    .bg(linear_gradient(180.0, linear_color_stop(c(CELL_TOP), 0.0), linear_color_stop(c(CELL_BOTTOM), 1.0)))
                    .items_center()
                    .justify_center()
                    .px(zp(8.0))
                    .gap(zp(10.0))
                    .child(power_button(s, index, cx))
                    .child(binding_chip(s, index, true, cx)),
            )
            .child(
                div()
                    .flex()
                    .flex_none()
                    .h(zp(LEVEL_H))
                    .w_full()
                    .bg(c(BAR))
                    .justify_center()
                    .items_center()
                    .child(
                        div()
                            .flex()
                            .items_baseline()
                            .gap(zp(3.0))
                            .child(tabular(label(level_number(s.config.fader_volumes[index]), 12.0, 600.0, c(TEXT))))
                            .child(label("dB", 9.5, 500.0, c(FAINT))),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .w_full()
                    .flex_grow()
                    .min_h(px(0.0))
                    .bg(c(CELL))
                    .px(zp(8.0))
                    .pt(zp(10.0))
                    .pb(zp(8.0))
                    .gap(zp(6.0))
                    .child(self.fader(s, index, cx))
                    .child(binding_chip(s, index, false, cx)),
            )
    }

    /// The strip's tab: what it drives, and a press picks something else.
    fn name_tab(&self, s: &Mixer, index: usize, cx: &mut Context<Self>) -> impl IntoElement {
        let health = if s.is_live(index) {
            Some(Health::Good)
        } else if s.is_present(index) {
            Some(Health::Waiting)
        } else {
            None
        };
        let open = s.menu == Some(Menu::Assign(index));
        let key = format!("assign-{index}");
        div().flex().flex_none().h(zp(TAB_H)).w_full().px(zp(6.0)).items_center().justify_center().bg(c(BAR)).child(
            div()
                .id(SharedString::from(key.clone()))
                .relative()
                .flex()
                .flex_col()
                .w_full()
                .px(zp(6.0))
                .py(zp(4.0))
                .gap(px(1.0))
                .items_center()
                .rounded(zp(6.0))
                .when(open, |tab| tab.bg(c(RAISED)))
                .hover(|style| style.bg(gpui::white().alpha(0.06)))
                .on_click(act(cx, move |s| s.menu = if s.menu == Some(Menu::Assign(index)) { None } else { Some(Menu::Assign(index)) }))
                .child(anchor(&self.anchors, key))
                .child(
                    div()
                        .flex()
                        .w_full()
                        .justify_center()
                        .items_center()
                        .gap(zp(5.0))
                        .child(dot(health.map(health_color), 5.0))
                        .child(label(s.strip_title(index), 12.0, 600.0, c(TEXT)).min_w(px(0.0)).truncate())
                        .child(label("▾", 9.0, 500.0, c(FAINT))),
                )
                .child(label(s.strip_detail(index), 10.0, 400.0, c(FAINT)).w_full().text_center().truncate()),
        )
    }

    fn fader(&self, s: &Mixer, index: usize, cx: &mut Context<Self>) -> impl IntoElement {
        let volume = s.config.fader_volumes[index];
        let muted = s.muted[index];
        let lit = s.touched_at[index].is_some_and(|at| at.elapsed() < ACTIVITY);
        let faders = self.faders.clone();
        // The whole width takes the drag, not just the handle.
        div()
            .id(SharedString::from(format!("fader-{index}")))
            .relative()
            .w_full()
            .flex_grow()
            .min_h(px(0.0))
            .on_mouse_down(MouseButton::Left, cx.listener(move |console, event, window, cx| console.fader_down(index, event, window, cx)))
            .child(
                canvas(
                    move |bounds, _, _| faders.borrow_mut()[index] = bounds,
                    move |bounds, _, window, _| paint_fader(bounds, volume, muted, lit, window),
                )
                .absolute()
                .size_full(),
            )
    }
}

/// A bordered button on one of the black bars.
fn bar_button(id: &'static str) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .flex()
        .flex_none()
        .h(zp(28.0))
        .px(zp(12.0))
        .items_center()
        .justify_center()
        .rounded(zp(6.0))
        .bg(c(CONTROL))
        .border_1()
        .border_color(c(HAIR))
}

/// A power symbol: a ring open at the top, and a bar through the gap.
fn power_icon(color: Hsla) -> impl IntoElement {
    canvas(
        |_, _, _| {},
        move |bounds, _, window, _| {
            let (x, y, r) = (bounds.origin.x + zp(8.0), bounds.origin.y + zp(8.5), z(6.0));
            let mut ring = PathBuilder::stroke(zp(1.8));
            for step in 0..=24 {
                let angle = (40.0 + 280.0 * step as f32 / 24.0f32).to_radians();
                let at = point(x + px(r * angle.sin()), y - px(r * angle.cos()));
                if step == 0 { ring.move_to(at) } else { ring.line_to(at) }
            }
            let mut bar = PathBuilder::stroke(zp(1.8));
            bar.move_to(point(x, bounds.origin.y + zp(1.5)));
            bar.line_to(point(x, bounds.origin.y + zp(8.0)));
            for path in [ring, bar] {
                if let Ok(path) = path.build() {
                    window.paint_path(path, color);
                }
            }
        },
    )
    .size(zp(16.0))
}

/// The strip's mute, as a power button: dark while the strip plays, glowing red while it's muted.
fn power_button(s: &Mixer, index: usize, cx: &mut Context<Console>) -> impl IntoElement {
    let muted = s.muted[index];
    let red = c(RED);
    let mut shadows = vec![BoxShadow { color: gpui::black().alpha(0.6), offset: point(px(0.0), zp(2.0)), blur_radius: zp(5.0), spread_radius: px(0.0) }];
    if muted {
        shadows.push(glow(red.alpha(0.55), 12.0));
    }
    div()
        .id(SharedString::from(format!("mute-{index}")))
        .flex()
        .flex_none()
        .size(zp(POWER))
        .rounded_full()
        .items_center()
        .justify_center()
        .bg(linear_gradient(180.0, linear_color_stop(c(0x48484C), 0.0), linear_color_stop(c(0x2A2A2D), 1.0)))
        .border(zp(2.0))
        .border_color(if muted { red } else { c(0x161618) })
        .shadow(shadows)
        .tooltip(tip(if muted { "Muted. Press to unmute." } else { "Playing. Press to mute." }))
        .on_click(act(cx, move |s| s.set_muted(index, !s.muted[index])))
        .child(power_icon(if muted { red } else { c(DIM) }))
}

/// A fader cap's rows from top to bottom, taken pixel by pixel from the reference console's cap:
/// a lit top face, fine ribs either side of a white line across the middle, and a black bottom
/// face. Each row is a point tall, so the cap keeps the reference's proportions.
const CAP_ROWS: [u32; 47] = [
    0x444444, 0x969696, 0x828282, 0x888888, 0x868686, 0x848484, 0x999999, 0x292929, 0x000000, 0x292929, 0x5E5E5E, 0x0C0C0C,
    0x222222, 0x161616, 0x6B6B6B, 0x5F5F5F, 0x1A1A1A, 0x2F2F2F, 0x3A3A3A, 0x8E8E8E, 0x5B5B5B, 0x2B2B2B, 0x636363, 0xE1E1E1,
    0xDEDEDE, 0xC5C5C5, 0x484848, 0x5E5E5E, 0x1C1C1C, 0x131313, 0x6C6C6C, 0x636363, 0x737373, 0x101010, 0x505050, 0x7D7D7D,
    0x7D7D7D, 0x737373, 0x2D2D2D, 0x828282, 0x9B9B9B, 0x363636, 0x080808, 0x111111, 0x111111, 0x111111, 0x111111,
];

/// The coloured line inside the groove, as the reference draws it: three columns, the middle one
/// brightest.
const FILL_LIT: [u32; 3] = [0x8A4A12, 0xFFC27A, 0xC2661A];
const FILL_MUTED: [u32; 3] = [0x3A3A3D, 0x9A9A9F, 0x5A5A5E];

/// Draws a fader into `bounds`: the lights, the groove with its lit line and cap, and the thin
/// activity line on the right. Edges land on whole device pixels, so nothing blurs at any zoom.
fn paint_fader(bounds: Bounds<Pixels>, volume: f32, muted: bool, lit: bool, window: &mut Window) {
    let scale = window.scale_factor();
    let snap = |value: f32| (value * scale).round() / scale;
    let rect = |x: f32, y: f32, w: f32, h: f32| Bounds::new(point(px(snap(x)), px(snap(y))), size(px(snap(x + w) - snap(x)), px(snap(y + h) - snap(y))));
    let height = f32::from(bounds.size.height);
    let (left, top0) = (f32::from(bounds.origin.x), f32::from(bounds.origin.y));
    let (top, bottom, at) = (top0 + handle_y(1.0, height), top0 + handle_y(0.0, height), top0 + handle_y(volume, height));
    let gap = z(14.0);
    let total = z(LED) + gap + z(HANDLE_W) + gap + z(2.0);
    let lights_x = left + (f32::from(bounds.size.width) - total) / 2.0;
    let track_x = lights_x + z(LED) + gap;
    let signal_x = track_x + z(HANDLE_W) + gap;
    let accent = c(ACCENT);

    // The lights, lit from the bottom up to the level; a muted strip's are grey. The bottom one
    // marks silence and stays dark.
    let on_color = if muted { c(DIM) } else { accent };
    for step in 0..LIGHTS {
        let y = top + (bottom - top) * step as f32 / (LIGHTS - 1) as f32;
        let floor = step == LIGHTS - 1;
        let on = !floor && volume > 0.0 && y >= at - 0.5;
        let light = rect(lights_x, y - z(LED / 2.0), z(LED), z(LED));
        let radius = Corners::all(px(z(LED / 2.0)));
        if on && !muted {
            window.paint_shadows(light, radius, &[BoxShadow { spread_radius: zp(1.0), ..glow(accent.alpha(0.75), 6.0) }]);
        }
        let off = if floor { c(0x0A0A0B) } else { c(LED_OFF) };
        window.paint_quad(fill(light, if on { on_color } else { off }).corner_radii(radius));
    }

    // The groove: a dark channel between two lit rims, running past both ends of the travel.
    let groove_x = track_x + z((HANDLE_W - GROOVE_W) / 2.0);
    let (groove_top, groove_h) = (top - z(GROOVE_W / 2.0), bottom - top + z(GROOVE_W));
    for (inset, color) in [(0.0, 0x1C1C1D), (1.0, 0x47474A), (2.0, 0x141415)] {
        let layer = rect(groove_x + z(inset), groove_top + z(inset), z(GROOVE_W - 2.0 * inset), groove_h - z(2.0 * inset));
        window.paint_quad(fill(layer, c(color)).corner_radii(Corners::all(px(z(GROOVE_W / 2.0 - inset)))));
    }

    // The level, as a line up the groove to the cap.
    let fill_x = track_x + z((HANDLE_W - 3.0) / 2.0);
    for (column, color) in (if muted { FILL_MUTED } else { FILL_LIT }).into_iter().enumerate() {
        window.paint_quad(fill(rect(fill_x + z(column as f32), at, z(1.0), (bottom - at).max(0.0)), c(color)));
    }

    // The cap, a row of the reference per point, with its shadow on the panel.
    let cap_top = snap(at - z(HANDLE_H / 2.0));
    let cap = rect(track_x, cap_top, z(HANDLE_W), z(HANDLE_H));
    window.paint_shadows(
        cap,
        Corners::all(px(0.0)),
        &[BoxShadow { color: gpui::black().alpha(0.6), offset: point(px(0.0), zp(1.0)), blur_radius: zp(3.0), spread_radius: zp(1.0) }],
    );
    let row_h = z(HANDLE_H) / CAP_ROWS.len() as f32;
    for (row, color) in CAP_ROWS.into_iter().enumerate() {
        let (from, to) = (snap(cap_top + row as f32 * row_h), snap(cap_top + (row + 1) as f32 * row_h));
        window.paint_quad(fill(rect(track_x, from, z(HANDLE_W), to - from), c(color)));
    }

    // The thin line on the right, in two segments: both light while the controller or a drag
    // moves the strip, and the lower one glows red while it's muted.
    let split = top + (bottom - top) * 0.42;
    let mut segment = |from: f32, to: f32, color: Option<Hsla>| {
        let line = rect(signal_x, from, z(2.0), (to - from).max(0.0));
        let radius = Corners::all(px(z(1.0)));
        if let Some(color) = color {
            window.paint_shadows(line, radius, &[glow(color.alpha(0.7), 6.0)]);
        }
        window.paint_quad(fill(line, color.unwrap_or(c(LED_OFF))).corner_radii(radius));
    };
    segment(top, split - z(3.0), lit.then_some(accent));
    segment(split + z(3.0), bottom, if muted { Some(c(RED)) } else { lit.then_some(accent) });
}

/// The control a strip's mute or fader learned, under the power button or the fader. A press
/// learns a new one; a right-click unbinds it.
fn binding_chip(s: &Mixer, index: usize, mute: bool, cx: &mut Context<Console>) -> impl IntoElement {
    let learn = if mute { Learn::Mute(index) } else { Learn::Fader(index) };
    let learning = s.learning == Some(learn);
    let bound = if mute { s.config.mute_controls[index] } else { s.config.fader_controls[index] };
    let accent = c(ACCENT);
    let (value, color) = if learning {
        ("Listening…".to_owned(), accent)
    } else {
        match bound {
            Some(control) => (control.label(), c(0xD4D4D7)),
            None => ("Learn".to_owned(), c(FAINT)),
        }
    };
    let kind = if mute { "mute" } else { "fader" };
    div()
        .id(SharedString::from(format!("{kind}-learn-{index}")))
        .flex()
        .flex_none()
        .w_full()
        .h(zp(20.0))
        .px(zp(6.0))
        .items_center()
        .justify_center()
        .rounded(zp(4.0))
        .when(learning, |chip| chip.bg(accent.alpha(0.14)))
        .hover(|style| style.bg(gpui::white().alpha(0.05)))
        .tooltip(tip(format!("The {kind} control. Press, then move a fader or press a button on the controller. Right-click to unbind.")))
        .on_click(act(cx, move |s| s.learning = if s.learning == Some(learn) { None } else { Some(learn) }))
        .on_mouse_down(
            MouseButton::Right,
            cx.listener(move |console, _, _, cx| {
                let mut s = console.mixer.borrow_mut();
                let list = if mute { &mut s.config.mute_controls } else { &mut s.config.fader_controls };
                list[index] = None;
                s.learning = None;
                s.schedule_save();
                cx.notify();
            }),
        )
        .child(label(value, 9.5, 500.0, color).min_w(px(0.0)).truncate())
}

fn footer(s: &Mixer, cx: &mut Context<Console>) -> impl IntoElement {
    let (status, color) = match (&s.error, s.updates.state()) {
        (Some(error), _) => (error.clone(), c(0xFF8A8E)),
        (None, UpdateState::Failed { message }) => (message, c(0xFF8A8E)),
        _ => (format!("{}  ·  {}", s.pipewire_status, s.midi_status), c(FAINT)),
    };
    let tray = s.config.close_to_tray;
    let knob = z(10.0);
    div()
        .flex()
        .flex_none()
        .h(zp(36.0))
        .w_full()
        .px(zp(14.0))
        .gap(zp(12.0))
        .items_center()
        .bg(c(BAR))
        .child(label(status, 11.0, 400.0, color).flex_grow().min_w(px(0.0)).truncate())
        .child(
            div()
                .id("tray")
                .flex()
                .flex_none()
                .h(zp(24.0))
                .px(zp(6.0))
                .gap(zp(7.0))
                .items_center()
                .rounded(zp(5.0))
                .hover(|style| style.bg(gpui::white().alpha(0.05)))
                .tooltip(tip(if s.tray_available { "Closing the window keeps the mixer running in the tray." } else { "No system tray, so closing the window quits." }))
                .on_click(act(cx, |s| {
                    s.config.close_to_tray = !s.config.close_to_tray;
                    s.schedule_save();
                }))
                .child(
                    div()
                        .relative()
                        .w(zp(24.0))
                        .h(zp(14.0))
                        .rounded_full()
                        .bg(if tray { c(ACCENT) } else { c(RAISED) })
                        .child(div().absolute().left(px(if tray { z(12.0) } else { z(2.0) })).top(zp(2.0)).size(px(knob)).rounded_full().bg(gpui::white())),
                )
                .child(label("Close to tray", 11.0, 500.0, if s.tray_available { c(DIM) } else { c(FAINT) })),
        )
        .child(
            bar_button("rescan")
                .h(zp(24.0))
                .px(zp(10.0))
                .hover(|style| style.bg(c(0x252527)))
                .on_click(act(cx, |s| {
                    s.error = None;
                    s.rescan();
                    s.refresh_midi();
                }))
                .child(label("Rescan", 11.0, 500.0, c(DIM))),
        )
}

/// A strip of the window under the header once an update is installed, or, for a copy that
/// can't install it, released.
fn update_banner(s: &Mixer, cx: &mut Context<Console>) -> Option<impl IntoElement> {
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
        div()
            .flex()
            .flex_none()
            .h(zp(38.0))
            .w_full()
            .px(zp(14.0))
            .gap(zp(10.0))
            .items_center()
            .bg(c(0x1E1710))
            .child(dot(Some(accent), 6.0).shadow(vec![glow(accent.alpha(0.7), 6.0)]))
            .child(label(message, 12.0, 500.0, c(TEXT)).flex_grow().min_w(px(0.0)).truncate())
            .child(
                div()
                    .id("update-action")
                    .flex()
                    .flex_none()
                    .h(zp(26.0))
                    .px(zp(12.0))
                    .items_center()
                    .justify_center()
                    .rounded(zp(6.0))
                    .bg(accent)
                    .on_click(cx.listener(move |console, _, _, cx| {
                        if !ready {
                            cx.open_url(RELEASES_URL);
                        } else if console.mixer.borrow().updates.restart() {
                            console.closing();
                            console.exit.set(Exit::Quit);
                            cx.quit();
                        }
                    }))
                    .child(label(if ready { "Restart" } else { "Download" }, 11.0, 600.0, c(0x111112))),
            )
            .child(
                div()
                    .id("update-dismiss")
                    .flex()
                    .flex_none()
                    .size(zp(26.0))
                    .items_center()
                    .justify_center()
                    .rounded(zp(6.0))
                    .hover(|style| style.bg(gpui::white().alpha(0.06)))
                    .on_click(act(cx, move |s| s.update_dismissed = Some(dismissed.clone())))
                    .child(label("×", 14.0, 500.0, c(DIM))),
            ),
    )
}

/// A menu row: its id, label, light, and whether it's the current choice.
type MenuItem = (String, String, Option<Health>, bool);

fn menu_panel(s: &Mixer, menu: Menu, anchors: &Rc<RefCell<HashMap<String, Bounds<Pixels>>>>, cx: &mut Context<Console>) -> impl IntoElement {
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
    let mut list = div().id("items").flex().flex_col().gap(px(1.0)).p(zp(4.0)).overflow_y_scroll();
    for (position, (id, name, health, chosen)) in items.into_iter().enumerate() {
        list = list.child(
            div()
                .id(("item", position))
                .flex()
                .flex_none()
                .h(zp(28.0))
                .px(zp(8.0))
                .gap(zp(8.0))
                .items_center()
                .rounded(zp(5.0))
                .hover(|style| style.bg(gpui::white().alpha(0.06)))
                .on_click(act(cx, move |s| {
                    match menu {
                        Menu::Assign(index) => s.assign(index, &id),
                        Menu::Midi => s.choose_port(&id),
                    }
                    s.menu = None;
                }))
                .child(dot(health.map(health_color), 6.0))
                .child(label(name, 12.0, if chosen { 600.0 } else { 400.0 }, if chosen { c(TEXT) } else { c(DIM) }).flex_grow().min_w(px(0.0)).truncate())
                .child(label(if chosen { "✓" } else { "" }, 11.0, 600.0, c(ACCENT))),
        );
    }
    let at = anchors.borrow().get(&anchor).copied().unwrap_or_default();
    anchored().position(point(at.origin.x, at.origin.y + at.size.height + zp(4.0))).snap_to_window_with_margin(zp(8.0)).child(
        div()
            .occlude()
            .flex()
            .flex_col()
            .w(zp(240.0))
            .max_h(zp(340.0))
            .rounded(zp(8.0))
            .bg(c(0x222224))
            .border_1()
            .border_color(c(0x3A3A3D))
            .shadow(vec![BoxShadow { color: gpui::black().alpha(0.6), offset: point(px(0.0), zp(8.0)), blur_radius: zp(24.0), spread_radius: px(0.0) }])
            .overflow_hidden()
            .child(list),
    )
}

/// The scenes `--demo` can open, for screenshots.
pub(crate) const SCENES: [&str; 6] = ["default", "assign-menu", "midi-menu", "learning", "update", "disconnected"];

/// The made-up state a `--demo` window shows.
pub(crate) fn demo(scene: &str) -> Option<Mixer> {
    let mut s = Mixer::new(Config::default());
    if scene != "disconnected" {
        demo_state(&mut s);
    }
    match scene {
        "default" => {}
        "assign-menu" => s.menu = Some(Menu::Assign(1)),
        "midi-menu" => s.menu = Some(Menu::Midi),
        "learning" => s.learning = Some(Learn::Mute(2)),
        "update" => s.updates.show(UpdateState::Ready { version: "26.12.0".into() }),
        "disconnected" => {
            s.pipewire_health = Health::Bad;
            s.pipewire_status = "PipeWire unavailable · pw-dump isn't installed; install the PipeWire tools".into();
            s.midi_status = "No MIDI device · plug one in".into();
        }
        _ => return None,
    }
    Some(s)
}

fn demo_state(s: &mut Mixer) {
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
        assert_eq!(level_number(1.0), "0.0");
        assert_eq!(level_number(0.5), "−18.1");
        assert_eq!(level_number(0.0), "−∞");
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

    #[test]
    fn every_scene_opens() {
        for scene in SCENES {
            assert!(demo(scene).is_some(), "{scene}");
        }
        assert!(demo("nope").is_none());
    }
}
