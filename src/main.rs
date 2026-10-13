mod app;
mod config;
mod engine;
mod stream;

fn main() -> Result<(), Box<dyn std::error::Error>> {
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
    if !instance.is_single() {
        return Ok(());
    }
    app::run()?;
    Ok(())
}
