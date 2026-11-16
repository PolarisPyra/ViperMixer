//! The window: five channel strips laid out like a hardware console. Each strip has a tab naming
//! what it drives, a power button that mutes it, its level, a fader beside a column of lights, and
//! the controller bindings it learned. A bigger window zooms everything up, and the strips and
//! faders take whatever is left over.
//!
//! It draws on the CPU into a shared-memory buffer (see `paint`), so it needs no GPU driver or
//! renderer in the process, and closing it to the tray drops the window with its buffer, fonts and
//! glyphs while the event loop keeps the mixer running.

use crate::config::{Config, FADER_COUNT, INPUT, MIDI_DISCONNECTED, OUTPUT, STEAM_ALL};
use crate::midi::{self, Control};
use crate::mixer::{Health, Learn, Menu, Mixer, port_title};
use crate::paint::{BLACK, Canvas, Fill, Rect, Rgba, Text, WHITE, rgb};
use crate::signal::Signal;
use crate::stream::{AudioStream, Device};
use crate::updates::{RELEASES_URL, UpdateState};
use std::cell::RefCell;
use std::num::NonZeroU32;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalSize, PhysicalPosition};
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{Key, NamedKey};
use winit::window::{Window, WindowId};

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
/// A line of text is this many times its size tall.
const LINE_HEIGHT: f32 = std::f32::consts::GOLDEN_RATIO;
/// How long the pointer rests on something before its tooltip shows.
const TIP_DELAY: Duration = Duration::from_millis(500);
/// Two presses this close in time and place make a double press.
const DOUBLE_PRESS: Duration = Duration::from_millis(400);
const DOUBLE_PRESS_DISTANCE: f32 = 5.0;

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
/// Menus and tooltips: a dark plate with a lighter edge.
const PLATE: u32 = 0x222224;
const PLATE_EDGE: u32 = 0x3A3A3D;

fn health_color(health: Health) -> Rgba {
    rgb(match health {
        Health::Good => 0x3DD68C,
        Health::Waiting => 0xF5A524,
        Health::Bad => RED,
    })
}

/// A level as the scale shows it, without its unit: the cubic volume PipeWire uses, in dB.
fn level_number(volume: f32) -> String {
    if volume <= 0.0001 { "−∞".into() } else { format!("{:.1}", 60.0 * volume.log10()).replace('-', "−") }
}

/// Where the handle's centre sits for `volume` in a fader `height` tall, at `unit` pixels a point.
fn handle_y(volume: f32, height: f32, unit: f32) -> f32 {
    unit * (FADER_PAD + HANDLE_H / 2.0) + (1.0 - volume) * travel(height, unit)
}

fn travel(height: f32, unit: f32) -> f32 {
    (height - unit * (2.0 * FADER_PAD + HANDLE_H)).max(1.0)
}

/// `rect` with its edges on whole pixels.
fn snap(rect: Rect) -> Rect {
    let (x, y) = (rect.x.round(), rect.y.round());
    Rect::new(x, y, rect.right().round() - x, rect.bottom().round() - y)
}

/// The update banner's version, and whether it's installed (rather than only released).
fn banner_state(s: &Mixer) -> Option<(String, bool)> {
    let (version, ready) = match s.updates.state() {
        UpdateState::Ready { version } => (version, true),
        UpdateState::Available { version } => (version, false),
        _ => return None,
    };
    (s.update_dismissed.as_deref() != Some(version.as_str())).then_some((version, ready))
}

/// Something in the window the pointer can press or rest on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Target {
    Pipewire,
    Midi,
    Updates,
    UpdateAction,
    UpdateDismiss,
    Tab(usize),
    Mute(usize),
    /// A strip's mute (true) or fader binding.
    Chip(usize, bool),
    Fader(usize),
    Tray,
    Rescan,
    /// Everything outside an open menu, which closes it.
    Backdrop,
    Panel,
    Item(usize),
}

struct Hit {
    rect: Rect,
    target: Target,
    tip: Option<String>,
}

/// Where things were drawn in the last frame, for the pointer to find them.
#[derive(Default)]
struct Layout {
    /// Bottom to top.
    hits: Vec<Hit>,
    faders: [Rect; FADER_COUNT],
    tabs: [Rect; FADER_COUNT],
    midi: Rect,
    panel: Rect,
    /// The open menu's choices, by row, and how far its rows can scroll.
    items: Vec<String>,
    scroll_room: f32,
    /// Pixels per point at the window's zoom.
    unit: f32,
}

impl Layout {
    fn at(&self, x: f32, y: f32) -> Option<&Hit> {
        self.hits.iter().rev().find(|hit| hit.rect.contains(x, y))
    }
}

/// How the window closed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Exit {
    /// To the tray: the mixer keeps running without it.
    Hide,
    Quit,
}

/// Runs the mixer until it quits, with its window open at first. A `demo` window never saves.
pub(crate) fn run(mixer: Rc<RefCell<Mixer>>, signal: Signal, demo: bool) {
    let event_loop = match EventLoop::new() {
        Ok(event_loop) => event_loop,
        Err(error) => {
            eprintln!("Couldn't open the window: {error}");
            return;
        }
    };
    let proxy = event_loop.create_proxy();
    signal.set_waker(Some(Arc::new(move || {
        let _ = proxy.send_event(());
    })));
    let mut app = App { mixer, signal: signal.clone(), demo, view: None, started: false };
    if let Err(error) = event_loop.run_app(&mut app) {
        eprintln!("The window stopped: {error}");
    }
    signal.set_waker(None);
}

struct App {
    mixer: Rc<RefCell<Mixer>>,
    signal: Signal,
    demo: bool,
    /// The open window; `None` while the mixer runs in the tray.
    view: Option<View>,
    started: bool,
}

/// An open window and what the pointer is doing in it.
struct View {
    window: Rc<Window>,
    surface: softbuffer::Surface<Rc<Window>, Rc<Window>>,
    text: Text,
    layout: Layout,
    cursor: Option<(f32, f32)>,
    hover: Option<Target>,
    hover_since: Instant,
    /// The tooltip showing, and where the pointer was when it appeared.
    tip: Option<((f32, f32), String)>,
    pressed: Option<Target>,
    left_down: bool,
    drag: Option<FaderDrag>,
    last_press: Option<(Instant, f32, f32)>,
    /// How far the open menu's rows are scrolled, and which menu that was for.
    scroll: f32,
    scrolled_menu: Option<Menu>,
}

/// A fader being dragged: where the pointer went down and the level it had then.
#[derive(Clone, Copy)]
struct FaderDrag {
    index: usize,
    start_y: f32,
    start: f32,
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if !self.started {
            self.started = true;
            self.open(event_loop);
        }
    }

    fn user_event(&mut self, _: &ActiveEventLoop, (): ()) {}

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        self.poll(event_loop);
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => {
                if self.closing() == Exit::Hide {
                    self.view = None;
                } else {
                    event_loop.exit();
                }
            }
            WindowEvent::RedrawRequested => self.redraw(),
            WindowEvent::Resized(_) | WindowEvent::ScaleFactorChanged { .. } => self.request_redraw(),
            WindowEvent::CursorMoved { position, .. } => self.moved(position),
            WindowEvent::CursorLeft { .. } => {
                if let Some(view) = &mut self.view {
                    view.cursor = None;
                    view.tip = None;
                    view.hover = None;
                    view.window.request_redraw();
                }
            }
            WindowEvent::MouseInput { state, button, .. } => self.button(event_loop, state, button),
            WindowEvent::MouseWheel { delta, .. } => self.wheel(delta),
            WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Pressed && event.logical_key == Key::Named(NamedKey::Escape) => {
                let mut s = self.mixer.borrow_mut();
                if s.learning.is_some() || s.menu.is_some() {
                    s.learning = None;
                    s.menu = None;
                    drop(s);
                    self.request_redraw();
                }
            }
            _ => {}
        }
    }
}

impl App {
    fn open(&mut self, event_loop: &ActiveEventLoop) {
        use winit::platform::wayland::WindowAttributesExtWayland;
        use winit::platform::x11::WindowAttributesExtX11;
        let mut attributes = Window::default_attributes()
            .with_title("Viper Mixer")
            .with_inner_size(LogicalSize::new(BASE[0], BASE[1]))
            .with_min_inner_size(LogicalSize::new(MIN[0], MIN[1]));
        attributes = WindowAttributesExtWayland::with_name(attributes, "viper-mixer", "viper-mixer");
        attributes = WindowAttributesExtX11::with_name(attributes, "viper-mixer", "viper-mixer");
        // Centred where the display server lets a window choose (X11).
        if let Some(monitor) = event_loop.primary_monitor() {
            let (size, at, scale) = (monitor.size(), monitor.position(), monitor.scale_factor());
            let x = at.x + (size.width as i32 - (f64::from(BASE[0]) * scale) as i32) / 2;
            let y = at.y + (size.height as i32 - (f64::from(BASE[1]) * scale) as i32) / 2;
            attributes = attributes.with_position(PhysicalPosition::new(x.max(at.x), y.max(at.y)));
        }
        let opened = event_loop.create_window(attributes).map_err(|error| error.to_string()).and_then(|window| {
            let window = Rc::new(window);
            let context = softbuffer::Context::new(window.clone()).map_err(|error| error.to_string())?;
            let surface = softbuffer::Surface::new(&context, window.clone()).map_err(|error| error.to_string())?;
            Ok((window, surface))
        });
        let (window, surface) = match opened {
            Ok(opened) => opened,
            Err(error) => {
                eprintln!("Couldn't open the window: {error}");
                event_loop.exit();
                return;
            }
        };
        window.request_redraw();
        self.view = Some(View {
            window,
            surface,
            text: Text::load(),
            layout: Layout::default(),
            cursor: None,
            hover: None,
            hover_since: Instant::now(),
            tip: None,
            pressed: None,
            left_down: false,
            drag: None,
            last_press: None,
            scroll: 0.0,
            scrolled_menu: None,
        });
    }

    /// The window is closing: to the tray, unless that's off or there's no tray.
    fn closing(&mut self) -> Exit {
        let mut s = self.mixer.borrow_mut();
        s.menu = None;
        s.learning = None;
        if !self.demo {
            s.save_now();
        }
        if s.config.close_to_tray && s.tray_available && !self.demo { Exit::Hide } else { Exit::Quit }
    }

    fn request_redraw(&self) {
        if let Some(view) = &self.view {
            view.window.request_redraw();
        }
    }

    /// Reads what the workers sent, runs the mixer's periodic work, and sleeps until it's next due.
    fn poll(&mut self, event_loop: &ActiveEventLoop) {
        let pending = self.signal.take();
        if pending.quit {
            if self.view.is_some() {
                self.closing();
            }
            event_loop.exit();
            return;
        }
        if pending.show {
            match &self.view {
                Some(view) => view.window.focus_window(),
                None => self.open(event_loop),
            }
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
        drop(s);
        if let Some(view) = &mut self.view {
            if view.tip.is_none() && !view.left_down {
                let resting = view.cursor.zip(view.hover).and_then(|(at, hover)| {
                    view.layout.hits.iter().rev().find(|hit| hit.target == hover).and_then(|hit| hit.tip.clone()).map(|tip| (at, tip))
                });
                if let Some(tip) = resting {
                    match (view.hover_since + TIP_DELAY).checked_duration_since(now) {
                        Some(left) if !left.is_zero() => next = next.min(left),
                        _ => {
                            view.tip = Some(tip);
                            changed = true;
                        }
                    }
                }
            }
            if changed {
                view.window.request_redraw();
            }
        }
        event_loop.set_control_flow(ControlFlow::WaitUntil(now + next));
    }

    fn redraw(&mut self) {
        let Some(view) = &mut self.view else { return };
        let size = view.window.inner_size();
        let (Some(width), Some(height)) = (NonZeroU32::new(size.width), NonZeroU32::new(size.height)) else { return };
        if view.surface.resize(width, height).is_err() {
            return;
        }
        let Ok(mut buffer) = view.surface.buffer_mut() else { return };
        let scale = view.window.scale_factor() as f32;
        let (w, h) = (size.width as f32, size.height as f32);
        let zoom = (w / scale / BASE[0]).min(h / scale / BASE[1]).max(1.0);
        let s = self.mixer.borrow();
        if view.scrolled_menu != s.menu {
            view.scrolled_menu = s.menu;
            view.scroll = 0.0;
        }
        view.layout = Layout { unit: scale * zoom, ..Layout::default() };
        let mut painter = Painter {
            canvas: Canvas::new(&mut buffer, size.width as usize, size.height as usize),
            text: &mut view.text,
            unit: scale * zoom,
            line: scale.round().max(1.0),
            hover: view.hover,
            layout: &mut view.layout,
            scroll: view.scroll,
        };
        painter.paint(&s, w, h, view.tip.as_ref());
        view.scroll = view.scroll.min(view.layout.scroll_room);
        view.window.pre_present_notify();
        let _ = buffer.present();
        drop(s);
        // What's under the pointer may have moved.
        self.rehover();
    }

    /// Finds what the pointer is on, redrawing when that changes.
    fn rehover(&mut self) {
        let Some(view) = &mut self.view else { return };
        let hover = view.cursor.and_then(|(x, y)| view.layout.at(x, y)).map(|hit| hit.target);
        if hover != view.hover {
            view.hover = hover;
            view.hover_since = Instant::now();
            view.tip = None;
            view.window.request_redraw();
        }
    }

    fn moved(&mut self, position: PhysicalPosition<f64>) {
        let Some(view) = &mut self.view else { return };
        let (x, y) = (position.x as f32, position.y as f32);
        view.cursor = Some((x, y));
        if let Some(drag) = view.drag {
            if view.left_down {
                let bounds = view.layout.faders[drag.index];
                let volume = drag.start - (y - drag.start_y) / travel(bounds.h, view.layout.unit);
                self.mixer.borrow_mut().set_volume(drag.index, volume);
                view.window.request_redraw();
            } else {
                view.drag = None;
            }
        }
        self.rehover();
    }

    fn button(&mut self, event_loop: &ActiveEventLoop, state: ElementState, button: MouseButton) {
        let Some(view) = &mut self.view else { return };
        let Some((x, y)) = view.cursor else { return };
        let target = view.layout.at(x, y).map(|hit| hit.target);
        let mut s = self.mixer.borrow_mut();
        match (button, state) {
            (MouseButton::Left, ElementState::Pressed) => {
                view.left_down = true;
                view.tip = None;
                view.window.request_redraw();
                if s.menu.is_some() && !matches!(target, Some(Target::Item(_) | Target::Panel)) {
                    s.menu = None;
                    return;
                }
                let now = Instant::now();
                let double = view.last_press.is_some_and(|(at, px, py)| now.duration_since(at) <= DOUBLE_PRESS && (x - px).hypot(y - py) <= DOUBLE_PRESS_DISTANCE);
                view.last_press = if double { None } else { Some((now, x, y)) };
                if let Some(Target::Fader(index)) = target {
                    let (bounds, unit) = (view.layout.faders[index], view.layout.unit);
                    let at = handle_y(s.config.fader_volumes[index], bounds.h, unit);
                    let y = y - bounds.y;
                    if double {
                        // A double press puts the fader back at unity.
                        s.set_volume(index, 1.0);
                    } else if (y - at).abs() > unit * HANDLE_H / 2.0 {
                        // Pressed beside the handle: it jumps there and drags from it.
                        s.set_volume(index, 1.0 - (y - unit * (FADER_PAD + HANDLE_H / 2.0)) / travel(bounds.h, unit));
                    }
                    view.drag = Some(FaderDrag { index, start_y: y + bounds.y, start: s.config.fader_volumes[index] });
                } else {
                    view.pressed = target;
                }
            }
            (MouseButton::Left, ElementState::Released) => {
                view.left_down = false;
                view.drag = None;
                let pressed = view.pressed.take();
                if let Some(target) = pressed.filter(|&pressed| target == Some(pressed)) {
                    drop(s);
                    self.click(event_loop, target);
                }
            }
            (MouseButton::Right, ElementState::Pressed) => {
                if let (None, Some(Target::Chip(index, mute))) = (s.menu, target) {
                    let list = if mute { &mut s.config.mute_controls } else { &mut s.config.fader_controls };
                    list[index] = None;
                    s.learning = None;
                    s.schedule_save();
                    view.window.request_redraw();
                }
            }
            _ => {}
        }
    }

    fn click(&mut self, event_loop: &ActiveEventLoop, target: Target) {
        let mut s = self.mixer.borrow_mut();
        match target {
            Target::Updates => s.updates.check_now(),
            Target::Midi => s.menu = if s.menu == Some(Menu::Midi) { None } else { Some(Menu::Midi) },
            Target::Tab(index) => s.menu = if s.menu == Some(Menu::Assign(index)) { None } else { Some(Menu::Assign(index)) },
            Target::Mute(index) => {
                let muted = !s.muted[index];
                s.set_muted(index, muted);
            }
            Target::Chip(index, mute) => {
                let learn = if mute { Learn::Mute(index) } else { Learn::Fader(index) };
                s.learning = if s.learning == Some(learn) { None } else { Some(learn) };
            }
            Target::Tray => {
                s.config.close_to_tray = !s.config.close_to_tray;
                s.schedule_save();
            }
            Target::Rescan => {
                s.error = None;
                s.rescan();
                s.refresh_midi();
            }
            Target::UpdateAction => match banner_state(&s) {
                Some((_, false)) => open_url(RELEASES_URL),
                Some((_, true)) if s.updates.restart() => {
                    drop(s);
                    self.closing();
                    event_loop.exit();
                    return;
                }
                _ => {}
            },
            Target::UpdateDismiss => s.update_dismissed = banner_state(&s).map(|(version, _)| version),
            Target::Item(row) => {
                let id = self.view.as_ref().and_then(|view| view.layout.items.get(row).cloned());
                match (s.menu, id) {
                    (Some(Menu::Assign(index)), Some(id)) => s.assign(index, &id),
                    (Some(Menu::Midi), Some(id)) => s.choose_port(&id),
                    _ => {}
                }
                s.menu = None;
            }
            Target::Pipewire | Target::Fader(_) | Target::Backdrop | Target::Panel => {}
        }
        drop(s);
        self.request_redraw();
    }

    fn wheel(&mut self, delta: MouseScrollDelta) {
        let Some(view) = &mut self.view else { return };
        let Some((x, y)) = view.cursor else { return };
        if self.mixer.borrow().menu.is_none() || !view.layout.panel.contains(x, y) {
            return;
        }
        let by = match delta {
            MouseScrollDelta::LineDelta(_, lines) => lines * 20.0 * view.layout.unit,
            MouseScrollDelta::PixelDelta(pixels) => pixels.y as f32,
        };
        view.scroll = (view.scroll - by).clamp(0.0, view.layout.scroll_room);
        view.window.request_redraw();
    }
}

/// Opens a link in the browser.
fn open_url(url: &str) {
    match std::process::Command::new("xdg-open").arg(url).spawn() {
        Ok(mut child) => {
            std::thread::spawn(move || child.wait());
        }
        Err(error) => eprintln!("Couldn't open {url}: {error}"),
    }
}

/// Draws one frame, noting where everything went.
struct Painter<'a, 'b> {
    canvas: Canvas<'b>,
    text: &'a mut Text,
    /// Pixels per point: the display's scale times the window's zoom.
    unit: f32,
    /// The lines between cells, which stay one point at any zoom.
    line: f32,
    hover: Option<Target>,
    layout: &'a mut Layout,
    scroll: f32,
}

/// A menu row: its id, label, light, and whether it's the current choice.
type MenuItem = (String, String, Option<Health>, bool);

impl Painter<'_, '_> {
    fn z(&self, size: f32) -> f32 {
        size * self.unit
    }

    fn hit(&mut self, rect: Rect, target: Target, tip: Option<String>) {
        self.layout.hits.push(Hit { rect, target, tip });
    }

    fn hovered(&self, target: Target) -> bool {
        self.hover == Some(target)
    }

    /// The baseline that centres a line of text at `size` on `center`.
    fn baseline(&mut self, center: f32, size: f32) -> f32 {
        let (ascent, descent) = self.text.metrics(self.z(size));
        (center + (ascent - descent) / 2.0).round()
    }

    fn width(&mut self, text: &str, size: f32) -> f32 {
        self.text.width(text, self.z(size), false)
    }

    fn fit(&mut self, text: &str, size: f32, max: f32) -> String {
        self.text.fit(text, self.z(size), max)
    }

    /// Draws text from `x`, returning where it ends.
    fn label(&mut self, text: &str, x: f32, baseline: f32, size: f32, color: Rgba) -> f32 {
        let size = self.z(size);
        self.text.draw(&mut self.canvas, text, x, baseline, size, color, false)
    }

    /// Text centred in `rect`, cut short to fit it less `pad` either side.
    fn centered(&mut self, text: &str, rect: Rect, pad: f32, size: f32, color: Rgba) {
        let text = self.fit(text, size, rect.w - 2.0 * pad);
        let width = self.width(&text, size);
        let baseline = self.baseline(rect.y + rect.h / 2.0, size);
        self.label(&text, rect.x + (rect.w - width) / 2.0, baseline, size, color);
    }

    /// A round light centred on `x, y`; unlit ones are a faint ring of the bar.
    fn dot(&mut self, x: f32, y: f32, size: f32, color: Option<Rgba>) -> Rect {
        let size = self.z(size);
        let rect = snap(Rect::new(x - size / 2.0, y - size / 2.0, size, size));
        self.canvas.round_rect(rect, size / 2.0, color.unwrap_or(rgb(HAIR)));
        rect
    }

    fn paint(&mut self, s: &Mixer, w: f32, h: f32, tip: Option<&((f32, f32), String)>) {
        self.canvas.clear(rgb(LINE));
        let header = snap(Rect::new(0.0, 0.0, w, self.z(46.0)));
        self.header(s, header);
        let mut top = header.bottom() + self.line;
        if let Some((version, ready)) = banner_state(s) {
            let banner = snap(Rect::new(0.0, top, w, self.z(38.0)));
            self.banner(banner, &version, ready);
            top = banner.bottom() + self.line;
        }
        let footer = snap(Rect::new(0.0, h - self.z(36.0), w, self.z(36.0)));
        self.footer(s, footer);
        let bottom = footer.y - self.line;
        // The gaps between strips show the lines.
        let span = (w - (FADER_COUNT - 1) as f32 * self.line) / FADER_COUNT as f32;
        for index in 0..FADER_COUNT {
            let left = (index as f32 * (span + self.line)).round();
            let right = (index as f32 * (span + self.line) + span).round();
            self.strip(s, index, left, right - left, top, bottom);
        }
        if let Some(menu) = s.menu {
            self.menu(s, menu, w, h);
        }
        if let Some(((x, y), tip)) = tip {
            self.tooltip(*x, *y, tip, w, h);
        }
    }

    fn header(&mut self, s: &Mixer, area: Rect) {
        self.canvas.rect(area, rgb(BAR));
        let center = area.y + area.h / 2.0;
        let baseline = self.baseline(center, 13.5);
        let end = self.label("Viper Mixer", area.x + self.z(14.0), baseline, 13.5, rgb(TEXT));
        let title_end = self.label(env!("CARGO_PKG_VERSION"), end + self.z(10.0), baseline, 11.0, rgb(FAINT));

        // From the right: the update button, the MIDI menu, which gives up width first, and PipeWire.
        let updates = snap(Rect::new(area.right() - self.z(14.0 + UPDATES_W), center - self.z(14.0), self.z(UPDATES_W), self.z(28.0)));
        let badge_w = self.width("PipeWire", 11.0) + self.z(20.0) + 2.0 * self.line;
        let room = updates.x - self.z(10.0) - (title_end + self.z(20.0) + badge_w + self.z(10.0));
        let midi_w = room.min(self.z(210.0)).max(self.z(120.0));
        let midi = snap(Rect::new(updates.x - self.z(10.0) - midi_w, center - self.z(14.0), midi_w, self.z(28.0)));
        let badge = snap(Rect::new(midi.x - self.z(10.0) - badge_w, center - self.z(12.0), badge_w, self.z(24.0)));

        let health = health_color(s.pipewire_health);
        self.canvas.round_rect(badge, self.z(6.0), health.alpha(0.1));
        self.canvas.border(badge, self.z(6.0), self.line, health);
        self.centered("PipeWire", badge, 0.0, 11.0, health);
        self.hit(badge, Target::Pipewire, Some(s.pipewire_status.clone()));

        let port = if s.connected_port.is_empty() {
            match s.config.midi_port.as_str() {
                "" => "Automatic",
                MIDI_DISCONNECTED => "Off",
                saved => port_title(saved),
            }
        } else {
            port_title(&s.connected_port)
        };
        let open = s.menu == Some(Menu::Midi);
        let fill = if self.hovered(Target::Midi) { 0x252527 } else { CONTROL };
        self.canvas.round_rect(midi, self.z(6.0), rgb(fill));
        self.canvas.border(midi, self.z(6.0), self.line, rgb(if open { ACCENT } else { HAIR }));
        self.hit(midi, Target::Midi, Some(s.midi_status.clone()));
        self.layout.midi = midi;
        let x = midi.x + self.line + self.z(10.0);
        self.dot(x + self.z(3.0), center, 6.0, Some(health_color(s.midi_health())));
        let arrow_x = midi.right() - self.line - self.z(10.0) - self.width("▾", 10.0);
        let arrow_baseline = self.baseline(center, 10.0);
        self.label("▾", arrow_x, arrow_baseline, 10.0, rgb(FAINT));
        let baseline = self.baseline(center, 12.0);
        let x = self.label("MIDI", x + self.z(14.0), baseline, 10.0, rgb(FAINT)) + self.z(8.0);
        let port = self.fit(port, 12.0, arrow_x - self.z(8.0) - x);
        self.label(&port, x, baseline, 12.0, rgb(TEXT));

        let update = s.updates.state();
        let checking = matches!(update, UpdateState::Checking | UpdateState::Downloading { .. });
        let label = match &update {
            UpdateState::Checking => "Checking…".to_owned(),
            UpdateState::UpToDate => "Up to date".to_owned(),
            UpdateState::Downloading { percent: Some(percent), .. } => format!("Downloading {percent}%"),
            UpdateState::Downloading { percent: None, .. } => "Downloading…".to_owned(),
            _ => "Check for updates".to_owned(),
        };
        let fill = if !checking && self.hovered(Target::Updates) { WHITE.alpha(0.05) } else { rgb(CONTROL) };
        self.canvas.round_rect(updates, self.z(6.0), fill);
        self.canvas.border(updates, self.z(6.0), self.line, rgb(HAIR));
        self.centered(&label, updates, self.z(12.0), 11.0, rgb(TEXT));
        if !checking {
            self.hit(updates, Target::Updates, None);
        }
    }

    /// A strip of the window under the header once an update is installed, or, for a copy that
    /// can't install it, released.
    fn banner(&mut self, area: Rect, version: &str, ready: bool) {
        let accent = rgb(ACCENT);
        self.canvas.rect(area, rgb(0x1E1710));
        let center = area.y + area.h / 2.0;
        let dot = snap(Rect::new(area.x + self.z(14.0), center - self.z(3.0), self.z(6.0), self.z(6.0)));
        self.canvas.shadow(dot, self.z(3.0), self.z(6.0), 0.0, 0.0, accent.alpha(0.7));
        self.canvas.round_rect(dot, self.z(3.0), accent);

        let dismiss = snap(Rect::new(area.right() - self.z(14.0 + 26.0), center - self.z(13.0), self.z(26.0), self.z(26.0)));
        if self.hovered(Target::UpdateDismiss) {
            self.canvas.round_rect(dismiss, self.z(6.0), WHITE.alpha(0.06));
        }
        self.centered("×", dismiss, 0.0, 14.0, rgb(DIM));
        self.hit(dismiss, Target::UpdateDismiss, None);

        let action_label = if ready { "Restart" } else { "Download" };
        let action_w = self.width(action_label, 11.0) + self.z(24.0);
        let action = snap(Rect::new(dismiss.x - self.z(10.0) - action_w, center - self.z(13.0), action_w, self.z(26.0)));
        self.canvas.round_rect(action, self.z(6.0), accent);
        self.centered(action_label, action, 0.0, 11.0, rgb(0x111112));
        self.hit(action, Target::UpdateAction, None);

        let message = if ready { format!("Viper Mixer {version} is installed. Restart to use it.") } else { format!("Viper Mixer {version} is out.") };
        let x = dot.right() + self.z(10.0);
        let message = self.fit(&message, 12.0, action.x - self.z(10.0) - x);
        let baseline = self.baseline(center, 12.0);
        self.label(&message, x, baseline, 12.0, rgb(TEXT));
    }

    fn strip(&mut self, s: &Mixer, index: usize, x: f32, w: f32, top: f32, bottom: f32) {
        let tab = snap(Rect::new(x, top, w, self.z(TAB_H)));
        let mute = snap(Rect::new(x, tab.bottom() + self.line, w, self.z(MUTE_CELL_H)));
        let level = snap(Rect::new(x, mute.bottom() + self.line, w, self.z(LEVEL_H)));
        let fader = Rect::new(x, level.bottom() + self.line, w, bottom - level.bottom() - self.line);
        self.name_tab(s, index, tab);

        self.canvas.round_rect(mute, 0.0, Fill::Vertical(rgb(CELL_TOP), rgb(CELL_BOTTOM)));
        let content_top = mute.y + (mute.h - self.z(POWER + 10.0 + 20.0)) / 2.0;
        self.power_button(s, index, mute.x + mute.w / 2.0, content_top + self.z(POWER / 2.0));
        let chip = snap(Rect::new(mute.x + self.z(8.0), content_top + self.z(POWER + 10.0), mute.w - self.z(16.0), self.z(20.0)));
        self.binding_chip(s, index, true, chip);

        self.canvas.rect(level, rgb(BAR));
        let number = level_number(s.config.fader_volumes[index]);
        let number_w = self.text.width(&number, self.z(12.0), true);
        let total = number_w + self.z(3.0) + self.width("dB", 9.5);
        let baseline = self.baseline(level.y + level.h / 2.0, 12.0);
        let x = level.x + (level.w - total) / 2.0;
        let size = self.z(12.0);
        let end = self.text.draw(&mut self.canvas, &number, x, baseline, size, rgb(TEXT), true);
        self.label("dB", end + self.z(3.0), baseline, 9.5, rgb(FAINT));

        self.canvas.rect(fader, rgb(CELL));
        let chip = snap(Rect::new(fader.x + self.z(8.0), fader.bottom() - self.z(8.0 + 20.0), fader.w - self.z(16.0), self.z(20.0)));
        let travel = snap(Rect::new(fader.x + self.z(8.0), fader.y + self.z(10.0), fader.w - self.z(16.0), chip.y - self.z(6.0) - fader.y - self.z(10.0)));
        self.layout.faders[index] = travel;
        // The whole width takes the drag, not just the handle.
        self.hit(travel, Target::Fader(index), None);
        let lit = s.touched_at[index].is_some_and(|at| at.elapsed() < ACTIVITY);
        self.fader(travel, s.config.fader_volumes[index], s.muted[index], lit);
        self.binding_chip(s, index, false, chip);
    }

    /// The strip's tab: what it drives, and a press picks something else.
    fn name_tab(&mut self, s: &Mixer, index: usize, tab: Rect) {
        self.canvas.rect(tab, rgb(BAR));
        let health = if s.is_live(index) {
            Some(Health::Good)
        } else if s.is_present(index) {
            Some(Health::Waiting)
        } else {
            None
        };
        let (title_h, detail_h) = (self.z(12.0) * LINE_HEIGHT, self.z(10.0) * LINE_HEIGHT);
        let inner_h = self.z(8.0) + title_h + self.line + detail_h;
        let button = snap(Rect::new(tab.x + self.z(6.0), tab.y + (tab.h - inner_h) / 2.0, tab.w - self.z(12.0), inner_h));
        let target = Target::Tab(index);
        if self.hovered(target) {
            self.canvas.round_rect(button, self.z(6.0), WHITE.alpha(0.06));
        } else if s.menu == Some(Menu::Assign(index)) {
            self.canvas.round_rect(button, self.z(6.0), rgb(RAISED));
        }
        self.hit(button, target, None);
        self.layout.tabs[index] = button;

        let center = button.y + self.z(4.0) + title_h / 2.0;
        let arrow_w = self.width("▾", 9.0);
        let room = button.w - self.z(12.0 + 5.0 + 5.0 + 5.0) - arrow_w;
        let title = self.fit(&s.strip_title(index), 12.0, room);
        let total = self.z(5.0 + 5.0) + self.width(&title, 12.0) + self.z(5.0) + arrow_w;
        let x = button.x + (button.w - total) / 2.0;
        self.dot(x + self.z(2.5), center, 5.0, health.map(health_color));
        let baseline = self.baseline(center, 12.0);
        let end = self.label(&title, x + self.z(10.0), baseline, 12.0, rgb(TEXT));
        let baseline = self.baseline(center, 9.0);
        self.label("▾", end + self.z(5.0), baseline, 9.0, rgb(FAINT));
        let detail = Rect::new(button.x, center + title_h / 2.0 + self.line, button.w, detail_h);
        self.centered(&s.strip_detail(index), detail, self.z(6.0), 10.0, rgb(FAINT));
    }

    /// The strip's mute, as a power button: dark while the strip plays, glowing red while it's muted.
    fn power_button(&mut self, s: &Mixer, index: usize, x: f32, y: f32) {
        let muted = s.muted[index];
        let red = rgb(RED);
        let radius = self.z(POWER / 2.0);
        let button = snap(Rect::new(x - radius, y - radius, 2.0 * radius, 2.0 * radius));
        self.canvas.shadow(button, radius, self.z(5.0), 0.0, self.z(2.0), BLACK.alpha(0.6));
        if muted {
            self.canvas.shadow(button, radius, self.z(12.0), 0.0, 0.0, red.alpha(0.55));
        }
        self.canvas.round_rect(button, radius, Fill::Vertical(rgb(0x48484C), rgb(0x2A2A2D)));
        self.canvas.border(button, radius, self.z(2.0), if muted { red } else { rgb(0x161618) });
        let tip = if muted { "Muted. Press to unmute." } else { "Playing. Press to mute." };
        self.hit(button, Target::Mute(index), Some(tip.into()));

        // A power symbol: a ring open at the top, and a bar through the gap.
        let color = if muted { red } else { rgb(DIM) };
        let (left, top) = (button.x + button.w / 2.0 - self.z(8.0), button.y + button.h / 2.0 - self.z(8.0));
        let (cx, cy, r) = (left + self.z(8.0), top + self.z(8.5), self.z(6.0));
        let ring: Vec<(f32, f32)> = (0..=24)
            .map(|step| {
                let angle = (40.0 + 280.0 * step as f32 / 24.0f32).to_radians();
                (cx + r * angle.sin(), cy - r * angle.cos())
            })
            .collect();
        self.canvas.stroke(&ring, self.z(1.8), color);
        self.canvas.stroke(&[(cx, top + self.z(1.5)), (cx, top + self.z(8.0))], self.z(1.8), color);
    }

    /// The control a strip's mute or fader learned, under the power button or the fader. A press
    /// learns a new one; a right-click unbinds it.
    fn binding_chip(&mut self, s: &Mixer, index: usize, mute: bool, chip: Rect) {
        let learn = if mute { Learn::Mute(index) } else { Learn::Fader(index) };
        let learning = s.learning == Some(learn);
        let bound = if mute { s.config.mute_controls[index] } else { s.config.fader_controls[index] };
        let accent = rgb(ACCENT);
        let (value, color) = if learning {
            ("Listening…".to_owned(), accent)
        } else {
            match bound {
                Some(control) => (control.label(), rgb(0xD4D4D7)),
                None => ("Learn".to_owned(), rgb(FAINT)),
            }
        };
        let target = Target::Chip(index, mute);
        if self.hovered(target) {
            self.canvas.round_rect(chip, self.z(4.0), WHITE.alpha(0.05));
        } else if learning {
            self.canvas.round_rect(chip, self.z(4.0), accent.alpha(0.14));
        }
        let kind = if mute { "mute" } else { "fader" };
        let tip = format!("The {kind} control. Press, then move a fader or press a button on the controller. Right-click to unbind.");
        self.hit(chip, target, Some(tip));
        self.centered(&value, chip, self.z(6.0), 9.5, color);
    }

    /// Draws a fader into `bounds`: the lights, the groove with its lit line and cap, and the thin
    /// activity line on the right. Edges land on whole pixels, so nothing blurs at any zoom.
    fn fader(&mut self, bounds: Rect, volume: f32, muted: bool, lit: bool) {
        let unit = self.unit;
        let z = |size: f32| size * unit;
        let rect = |x: f32, y: f32, w: f32, h: f32| snap(Rect::new(x, y, w, h));
        let (top, bottom, at) =
            (bounds.y + handle_y(1.0, bounds.h, unit), bounds.y + handle_y(0.0, bounds.h, unit), bounds.y + handle_y(volume, bounds.h, unit));
        let gap = z(14.0);
        let total = z(LED) + gap + z(HANDLE_W) + gap + z(2.0);
        let lights_x = bounds.x + (bounds.w - total) / 2.0;
        let track_x = lights_x + z(LED) + gap;
        let signal_x = track_x + z(HANDLE_W) + gap;
        let accent = rgb(ACCENT);

        // The lights, lit from the bottom up to the level; a muted strip's are grey. The bottom one
        // marks silence and stays dark.
        let on_color = if muted { rgb(DIM) } else { accent };
        for step in 0..LIGHTS {
            let y = top + (bottom - top) * step as f32 / (LIGHTS - 1) as f32;
            let floor = step == LIGHTS - 1;
            let on = !floor && volume > 0.0 && y >= at - 0.5;
            let light = rect(lights_x, y - z(LED / 2.0), z(LED), z(LED));
            if on && !muted {
                self.canvas.shadow(light, z(LED / 2.0), z(6.0), z(1.0), 0.0, accent.alpha(0.75));
            }
            let off = if floor { rgb(0x0A0A0B) } else { rgb(LED_OFF) };
            self.canvas.round_rect(light, z(LED / 2.0), if on { on_color } else { off });
        }

        // The groove: a dark channel between two lit rims, running past both ends of the travel.
        let groove_x = track_x + z((HANDLE_W - GROOVE_W) / 2.0);
        let (groove_top, groove_h) = (top - z(GROOVE_W / 2.0), bottom - top + z(GROOVE_W));
        for (inset, color) in [(0.0, 0x1C1C1D), (1.0, 0x47474A), (2.0, 0x141415)] {
            let layer = rect(groove_x + z(inset), groove_top + z(inset), z(GROOVE_W - 2.0 * inset), groove_h - z(2.0 * inset));
            self.canvas.round_rect(layer, z(GROOVE_W / 2.0 - inset), rgb(color));
        }

        // The level, as a line up the groove to the cap.
        let fill_x = track_x + z((HANDLE_W - 3.0) / 2.0);
        for (column, color) in (if muted { FILL_MUTED } else { FILL_LIT }).into_iter().enumerate() {
            self.canvas.rect(rect(fill_x + z(column as f32), at, z(1.0), (bottom - at).max(0.0)), rgb(color));
        }

        // The cap, a row of the reference per point, with its shadow on the panel.
        let cap_top = (at - z(HANDLE_H / 2.0)).round();
        let cap = rect(track_x, cap_top, z(HANDLE_W), z(HANDLE_H));
        self.canvas.shadow(cap, 0.0, z(3.0), z(1.0), z(1.0), BLACK.alpha(0.6));
        let row_h = z(HANDLE_H) / CAP_ROWS.len() as f32;
        for (row, color) in CAP_ROWS.into_iter().enumerate() {
            let (from, to) = ((cap_top + row as f32 * row_h).round(), (cap_top + (row + 1) as f32 * row_h).round());
            self.canvas.rect(Rect::new(cap.x, from, cap.w, to - from), rgb(color));
        }

        // The thin line on the right, in two segments: both light while the controller or a drag
        // moves the strip, and the lower one glows red while it's muted.
        let split = top + (bottom - top) * 0.42;
        let segment = |canvas: &mut Canvas, from: f32, to: f32, color: Option<Rgba>| {
            let line = rect(signal_x, from, z(2.0), (to - from).max(0.0));
            if let Some(color) = color {
                canvas.shadow(line, z(1.0), z(6.0), 0.0, 0.0, color.alpha(0.7));
            }
            canvas.round_rect(line, z(1.0), color.unwrap_or(rgb(LED_OFF)));
        };
        segment(&mut self.canvas, top, split - z(3.0), lit.then_some(accent));
        segment(&mut self.canvas, split + z(3.0), bottom, if muted { Some(rgb(RED)) } else { lit.then_some(accent) });
    }

    fn footer(&mut self, s: &Mixer, area: Rect) {
        self.canvas.rect(area, rgb(BAR));
        let center = area.y + area.h / 2.0;
        let (status, color) = match (&s.error, s.updates.state()) {
            (Some(error), _) => (error.clone(), rgb(0xFF8A8E)),
            (None, UpdateState::Failed { message }) => (message, rgb(0xFF8A8E)),
            _ => (format!("{}  ·  {}", s.pipewire_status, s.midi_status), rgb(FAINT)),
        };

        let rescan_w = self.width("Rescan", 11.0) + self.z(20.0) + 2.0 * self.line;
        let rescan = snap(Rect::new(area.right() - self.z(14.0) - rescan_w, center - self.z(12.0), rescan_w, self.z(24.0)));
        let fill = if self.hovered(Target::Rescan) { 0x252527 } else { CONTROL };
        self.canvas.round_rect(rescan, self.z(6.0), rgb(fill));
        self.canvas.border(rescan, self.z(6.0), self.line, rgb(HAIR));
        self.centered("Rescan", rescan, 0.0, 11.0, rgb(DIM));
        self.hit(rescan, Target::Rescan, None);

        let tray_w = self.z(6.0 + 24.0 + 7.0 + 6.0) + self.width("Close to tray", 11.0);
        let tray = snap(Rect::new(rescan.x - self.z(12.0) - tray_w, center - self.z(12.0), tray_w, self.z(24.0)));
        if self.hovered(Target::Tray) {
            self.canvas.round_rect(tray, self.z(5.0), WHITE.alpha(0.05));
        }
        let on = s.config.close_to_tray;
        let switch = snap(Rect::new(tray.x + self.z(6.0), center - self.z(7.0), self.z(24.0), self.z(14.0)));
        self.canvas.round_rect(switch, switch.h / 2.0, rgb(if on { ACCENT } else { RAISED }));
        let knob = snap(Rect::new(switch.x + self.z(if on { 12.0 } else { 2.0 }), switch.y + self.z(2.0), self.z(10.0), self.z(10.0)));
        self.canvas.round_rect(knob, knob.w / 2.0, WHITE);
        let baseline = self.baseline(center, 11.0);
        self.label("Close to tray", switch.right() + self.z(7.0), baseline, 11.0, rgb(if s.tray_available { DIM } else { FAINT }));
        let tip = if s.tray_available { "Closing the window keeps the mixer running in the tray." } else { "No system tray, so closing the window quits." };
        self.hit(tray, Target::Tray, Some(tip.into()));

        let x = area.x + self.z(14.0);
        let status = self.fit(&status, 11.0, tray.x - self.z(12.0) - x);
        self.label(&status, x, baseline, 11.0, color);
    }

    fn menu(&mut self, s: &Mixer, menu: Menu, w: f32, h: f32) {
        let (anchor, items) = match menu {
            Menu::Assign(index) => {
                let current = s.assignment(index).to_owned();
                let items: Vec<MenuItem> = s
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
                (self.layout.tabs[index], items)
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
                (self.layout.midi, items)
            }
        };
        self.hit(Rect::new(0.0, 0.0, w, h), Target::Backdrop, None);

        // Under its button, moved back inside the window if it would cross the edge.
        let rows = items.len() as f32;
        let content_h = self.z(8.0) + rows * self.z(28.0) + (rows - 1.0).max(0.0) * self.line;
        let (panel_w, panel_h) = (self.z(240.0), (content_h + 2.0 * self.line).min(self.z(340.0)));
        let margin = self.z(8.0);
        let x = anchor.x.min(w - margin - panel_w).max(margin);
        let y = (anchor.bottom() + self.z(8.0)).min(h - margin - panel_h).max(margin);
        let panel = snap(Rect::new(x, y, panel_w, panel_h));
        self.canvas.shadow(panel, self.z(8.0), self.z(24.0), 0.0, self.z(8.0), BLACK.alpha(0.6));
        self.canvas.round_rect(panel, self.z(8.0), rgb(PLATE));
        self.hit(panel, Target::Panel, None);
        self.layout.panel = panel;

        let inside = panel.inset(self.line);
        self.layout.scroll_room = (content_h - inside.h).max(0.0);
        let scroll = self.scroll.min(self.layout.scroll_room);
        self.canvas.set_clip(Some(inside));
        let mut y = inside.y + self.z(4.0) - scroll;
        for (row, (id, name, health, chosen)) in items.into_iter().enumerate() {
            let item = Rect::new(inside.x + self.z(4.0), y.round(), inside.w - self.z(8.0), self.z(28.0).round());
            y += self.z(28.0) + self.line;
            self.layout.items.push(id);
            let visible = Rect::new(item.x, item.y.max(inside.y), item.w, item.bottom().min(inside.bottom()) - item.y.max(inside.y));
            if visible.h <= 0.0 {
                continue;
            }
            let target = Target::Item(row);
            if self.hovered(target) {
                self.canvas.round_rect(item, self.z(5.0), WHITE.alpha(0.06));
            }
            self.hit(visible, target, None);
            let center = item.y + item.h / 2.0;
            self.dot(item.x + self.z(8.0 + 3.0), center, 6.0, health.map(health_color));
            let check_w = self.width("✓", 11.0);
            let x = item.x + self.z(8.0 + 6.0 + 8.0);
            let color = if chosen { rgb(TEXT) } else { rgb(DIM) };
            let name = self.fit(&name, 12.0, item.right() - self.z(8.0 + 8.0) - check_w - x);
            let baseline = self.baseline(center, 12.0);
            self.label(&name, x, baseline, 12.0, color);
            if chosen {
                let baseline = self.baseline(center, 11.0);
                self.label("✓", item.right() - self.z(8.0) - check_w, baseline, 11.0, rgb(ACCENT));
            }
        }
        self.canvas.set_clip(None);
        self.canvas.border(panel, self.z(8.0), self.line, rgb(PLATE_EDGE));
    }

    /// A tooltip: help on a dark plate by the pointer, wrapped to the window.
    fn tooltip(&mut self, x: f32, y: f32, tip: &str, w: f32, h: f32) {
        let (size, pad_x, pad_y) = (11.0, self.z(8.0), self.z(5.0));
        let max = w - 2.0 * pad_x - 2.0 * self.line - self.z(16.0);
        let mut lines: Vec<String> = Vec::new();
        for word in tip.split(' ') {
            match lines.last_mut() {
                Some(line) if self.width(&format!("{line} {word}"), size) <= max => {
                    line.push(' ');
                    line.push_str(word);
                }
                _ => lines.push(word.to_owned()),
            }
        }
        let line_h = self.z(size) * LINE_HEIGHT;
        let text_w = lines.iter().map(|line| self.width(line, size)).fold(0.0, f32::max);
        let (plate_w, plate_h) = (text_w + 2.0 * (pad_x + self.line), lines.len() as f32 * line_h + 2.0 * (pad_y + self.line));
        // At the pointer, or above and to its left when it would cross the edge.
        let left = if x + 1.0 + plate_w > w { x - plate_w - 1.0 } else { x + 1.0 };
        let top = if y + 1.0 + plate_h > h { y - plate_h - 1.0 } else { y + 1.0 };
        let plate = snap(Rect::new(left.max(0.0), top.max(0.0), plate_w, plate_h));
        self.canvas.round_rect(plate, self.z(5.0), rgb(PLATE));
        self.canvas.border(plate, self.z(5.0), self.line, rgb(PLATE_EDGE));
        for (row, line) in lines.iter().enumerate() {
            let baseline = self.baseline(plate.y + self.line + pad_y + (row as f32 + 0.5) * line_h, size);
            self.label(line, plate.x + self.line + pad_x, baseline, size, rgb(TEXT));
        }
    }
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
        assert_eq!(handle_y(1.0, 300.0, 1.0), FADER_PAD + HANDLE_H / 2.0);
        assert_eq!(handle_y(0.0, 300.0, 1.0), 300.0 - FADER_PAD - HANDLE_H / 2.0);
        assert_eq!(handle_y(0.0, 600.0, 2.0), 600.0 - 2.0 * (FADER_PAD + HANDLE_H / 2.0));
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

    #[test]
    fn the_topmost_target_takes_the_pointer() {
        let mut layout = Layout::default();
        layout.hits.push(Hit { rect: Rect::new(0.0, 0.0, 100.0, 100.0), target: Target::Backdrop, tip: None });
        layout.hits.push(Hit { rect: Rect::new(10.0, 10.0, 20.0, 20.0), target: Target::Item(0), tip: None });
        assert_eq!(layout.at(15.0, 15.0).map(|hit| hit.target), Some(Target::Item(0)));
        assert_eq!(layout.at(50.0, 50.0).map(|hit| hit.target), Some(Target::Backdrop));
        assert!(layout.at(150.0, 50.0).is_none());
    }
}
