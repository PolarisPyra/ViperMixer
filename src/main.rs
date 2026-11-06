//! Viper Mixer: hardware faders on a MIDI controller drive app and device volumes in PipeWire.

mod config;
mod engine;
mod instance;
mod midi;
mod mixer;
mod stream;
mod tray;
mod ui;

use config::Config;
use mixer::Mixer;

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
    // Any other argument is one of Rune's tooling flags (`--snap`, `--tree`, `--script`), which
    // draw the demo state without a window, so they skip the lock, the tray and saved settings.
    let tooling = !arguments.is_empty();
    let _instance = if tooling {
        None
    } else {
        match instance::acquire() {
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
        }
    };
    rune::set_app_id("viper-mixer");
    let app = rune::App::new(ui::spec())
        .init(move || Mixer::new(if tooling { Config::default() } else { Config::load() }))
        .keep_running()
        .on_close(ui::on_close);
    let handle = app.handle();
    let _tray = if tooling { None } else { tray::start(handle.clone()) };
    if _tray.is_some() {
        handle.update(|s, _| s.tray_available = true);
    }
    if !tooling {
        instance::listen(move |token| match token {
            Some(token) => handle.activate(token),
            None => handle.open(),
        });
    }
    app.run();
}
