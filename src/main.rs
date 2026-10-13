mod app;
mod config;
mod engine;
mod stream;

fn main() -> eframe::Result<()> {
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
    app::run()
}
