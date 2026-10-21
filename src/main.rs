mod app;
mod config;
mod engine;
mod stream;
mod tray;

pub(crate) fn startup_trace(stage: &str) {
    use std::sync::OnceLock;
    use std::time::Instant;

    static START: OnceLock<(Instant, bool)> = OnceLock::new();
    let (start, enabled) = START.get_or_init(|| {
        (
            Instant::now(),
            std::env::var_os("VIPER_STARTUP_TRACE").is_some(),
        )
    });
    if *enabled {
        eprintln!("[startup] +{:.3}s {stage}", start.elapsed().as_secs_f64());
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    startup_trace("process entry");
    if std::env::args().any(|argument| argument == "--list-midi") {
        println!("MIDI inputs:");
        match midir::MidiInput::new("Viper Mixer") {
            Ok(input) => {
                for port in input.ports() {
                    if let Ok(name) = input.port_name(&port) {
                        println!("  {name}");
                    }
                }
            }
            Err(error) => eprintln!("Could not list MIDI inputs: {error}"),
        }
        return Ok(());
    }
    let instance = single_instance::SingleInstance::new("viper-midi-mixer")?;
    startup_trace("single-instance lock acquired");
    if !instance.is_single() {
        return Ok(());
    }
    startup_trace("entering app startup");
    app::run()?;
    Ok(())
}
