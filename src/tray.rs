//! The tray icon: a click shows the window, and its menu shows the window or quits.

use crate::mixer::Mixer;
use rune::{AppHandle, Tray, TrayEvent, TrayIcon, TrayItem, TraySpec};

/// Starts the tray, or returns `None` where the desktop has none (closing the window then quits).
pub(crate) fn start(app: AppHandle<Mixer>) -> Option<Tray> {
    let icon = icon()?;
    let spec = TraySpec::new("viper-mixer", "Viper Mixer", icon).tooltip("Viper Mixer").menu(vec![
        TrayItem::action("show", "Show Viper Mixer"),
        TrayItem::Separator,
        TrayItem::action("quit", "Quit"),
    ]);
    let tray = Tray::new(spec, move |event| match event {
        TrayEvent::Activate { token: Some(token) } => app.activate(token),
        TrayEvent::Activate { token: None } => app.open(),
        TrayEvent::Menu(id) if id == "show" => app.open(),
        TrayEvent::Menu(id) if id == "quit" => app.update(|s, tx| {
            s.save_now();
            tx.quit();
        }),
        _ => {}
    });
    match tray {
        Ok(tray) => Some(tray),
        Err(error) => {
            eprintln!("No system tray ({error:?}); closing the window quits.");
            None
        }
    }
}

/// The logo at tray size, from `assets/tray.png` (see the README for regenerating it).
fn icon() -> Option<TrayIcon> {
    let mut decoder = png::Decoder::new(std::io::Cursor::new(include_bytes!("../assets/tray.png")));
    decoder.set_transformations(png::Transformations::normalize_to_color8() | png::Transformations::ALPHA);
    let mut reader = decoder.read_info().ok()?;
    let mut pixels = vec![0; reader.output_buffer_size()?];
    let frame = reader.next_frame(&mut pixels).ok()?;
    pixels.truncate(frame.buffer_size());
    (frame.color_type == png::ColorType::Rgba).then(|| TrayIcon::rgba(frame.width, frame.height, pixels))
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_tray_icon_decodes() {
        assert!(super::icon().is_some());
    }
}
