//! The tray icon: a click shows the window, and its menu shows the window or quits.

use crate::signal::Signal;
use ksni::blocking::{Handle, TrayMethods};
use ksni::menu::StandardItem;
use ksni::{Icon, MenuItem};

struct Tray {
    signal: Signal,
    icon: Icon,
}

impl ksni::Tray for Tray {
    fn id(&self) -> String {
        "viper-mixer".into()
    }

    fn title(&self) -> String {
        "Viper Mixer".into()
    }

    fn icon_pixmap(&self) -> Vec<Icon> {
        vec![self.icon.clone()]
    }

    fn tool_tip(&self) -> ksni::ToolTip {
        ksni::ToolTip { title: "Viper Mixer".into(), ..Default::default() }
    }

    fn activate(&mut self, _x: i32, _y: i32) {
        self.signal.show();
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        vec![
            StandardItem { label: "Show Viper Mixer".into(), activate: Box::new(|tray: &mut Self| tray.signal.show()), ..Default::default() }.into(),
            MenuItem::Separator,
            StandardItem { label: "Quit".into(), activate: Box::new(|tray: &mut Self| tray.signal.quit()), ..Default::default() }.into(),
        ]
    }
}

/// Starts the tray, or returns `None` where the desktop has none (closing the window then quits).
pub(crate) fn start(signal: Signal) -> Option<Handle<impl ksni::Tray>> {
    let icon = icon()?;
    match (Tray { signal, icon }).spawn() {
        Ok(handle) => Some(handle),
        Err(error) => {
            eprintln!("No system tray ({error}); closing the window quits.");
            None
        }
    }
}

/// The logo at tray size, from `assets/tray.png` (see the README for regenerating it), as the
/// ARGB the tray protocol wants.
fn icon() -> Option<Icon> {
    let mut decoder = png::Decoder::new(std::io::Cursor::new(include_bytes!("../assets/tray.png")));
    decoder.set_transformations(png::Transformations::normalize_to_color8() | png::Transformations::ALPHA);
    let mut reader = decoder.read_info().ok()?;
    let mut pixels = vec![0; reader.output_buffer_size()?];
    let frame = reader.next_frame(&mut pixels).ok()?;
    pixels.truncate(frame.buffer_size());
    if frame.color_type != png::ColorType::Rgba {
        return None;
    }
    let data = pixels.as_chunks::<4>().0.iter().flat_map(|&[r, g, b, a]| [a, r, g, b]).collect();
    Some(Icon { width: frame.width as i32, height: frame.height as i32, data })
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_tray_icon_decodes() {
        let icon = super::icon().unwrap();
        assert_eq!(icon.data.len(), (icon.width * icon.height * 4) as usize);
    }
}
