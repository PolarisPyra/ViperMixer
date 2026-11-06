//! Viper Mixer: hardware faders on a MIDI controller drive app and device volumes in PipeWire.

mod config;
mod engine;
mod instance;
mod midi;
mod mixer;
mod signal;
mod stream;
mod tray;
mod ui;
mod updates;

use config::Config;
use engine::Engine;
use mixer::Mixer;
use signal::Signal;
use std::cell::RefCell;
use std::rc::Rc;
use updates::Updater;

fn main() {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    if arguments.first().is_some_and(|argument| argument == "--list-midi") {
        match midi::list_ports() {
            Ok(ports) => ports.iter().for_each(|port| println!("{}", midi::stable_port_name(port))),
            Err(error) => {
                eprintln!("Couldn't list MIDI inputs: {error}");
                std::process::exit(1);
            }
        }
        return;
    }
    // `--demo [scene]` opens the window on made-up state, for screenshots: no lock, tray,
    // controller, PipeWire or saved settings.
    if arguments.first().is_some_and(|argument| argument == "--demo") {
        let scene = arguments.get(1).map_or("default", String::as_str);
        let Some(mixer) = ui::demo(scene) else {
            eprintln!("No scene called {scene}; the scenes are {}.", ui::SCENES.join(", "));
            std::process::exit(2);
        };
        ui::run(Rc::new(RefCell::new(mixer)), Signal::default(), true);
        return;
    }
    let _instance = match instance::acquire() {
        Ok(Some(instance)) => Some(instance),
        Ok(None) => {
            if let Err(error) = instance::show_running() {
                eprintln!("Viper Mixer is already running but didn't answer: {error}");
                std::process::exit(1);
            }
            return;
        }
        Err(error) => {
            eprintln!("Can't check for another Viper Mixer ({error}); starting anyway.");
            None
        }
    };
    let signal = Signal::default();
    let mixer = Rc::new(RefCell::new(Mixer::new(Config::load())));
    {
        let mut mixer = mixer.borrow_mut();
        mixer.updates = Updater::start(signal.waker());
        mixer.attach(Engine::start(signal.waker()));
    }
    let _tray = tray::start(signal.clone());
    mixer.borrow_mut().tray_available = _tray.is_some();
    let launches = signal.clone();
    instance::listen(move |_token| launches.show());
    // The window closes to the tray, where the mixer keeps following the controller until the
    // tray or another launch shows the window again.
    while ui::run(mixer.clone(), signal.clone(), false) == ui::Exit::Hide && ui::wait_hidden(&mut mixer.borrow_mut(), &signal) {}
    mixer.borrow_mut().save_now();
    // Restart pressed on the update banner: the new AppImage takes over this process. The lock
    // and the socket close with it, so the new copy starts as the only one.
    if let Some(appimage) = updates::restart_target() {
        use std::os::unix::process::CommandExt;
        let error = std::process::Command::new(appimage).exec();
        eprintln!("Couldn't start the update: {error}");
    }
}
