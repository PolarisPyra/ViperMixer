use image::DynamicImage;
use std::sync::mpsc::Sender;

/// Menu selections sent from the system tray to the app.
#[derive(Clone, Copy)]
pub(crate) enum TrayEvent {
    Show,
    Quit,
}

#[cfg(target_os = "linux")]
mod ksni_backend {
    use super::{DynamicImage, Sender, TrayEvent};
    use ksni::{
        blocking::{Handle, TrayMethods},
        menu::StandardItem,
        Icon, MenuItem, ToolTip,
    };

    /// StatusNotifierItem state read by the desktop host, which calls back on menu selections.
    pub(crate) struct LinuxTray {
        sender: Sender<TrayEvent>,
        icon: Vec<Icon>,
    }

    /// Build a menu entry that sends `event` when selected.
    ///
    /// # Arguments
    /// * `label` - Text shown in the menu.
    /// * `event` - Event sent when the entry is selected.
    fn entry(label: &str, event: TrayEvent) -> MenuItem<LinuxTray> {
        MenuItem::Standard(StandardItem {
            label: label.into(),
            activate: Box::new(move |tray: &mut LinuxTray| {
                let _ = tray.sender.send(event);
            }),
            ..Default::default()
        })
    }

    /// Convert the logo into the ARGB32 pixmap the StatusNotifierItem spec expects.
    ///
    /// # Arguments
    /// * `logo` - Source image, scaled down to at most 96 pixels per side.
    fn argb_icon(logo: &DynamicImage) -> Icon {
        let rgba = logo.thumbnail(96, 96).into_rgba8();
        let (width, height) = rgba.dimensions();
        Icon {
            width: width as i32,
            height: height as i32,
            data: rgba
                .pixels()
                .flat_map(|pixel| [pixel[3], pixel[0], pixel[1], pixel[2]])
                .collect(),
        }
    }

    impl ksni::Tray for LinuxTray {
        // Left click opens the menu instead of calling activate, so the menu is the only tray action.
        const MENU_ON_ACTIVATE: bool = true;

        fn id(&self) -> String {
            env!("CARGO_PKG_NAME").into()
        }

        fn title(&self) -> String {
            "Viper Mixer".into()
        }

        fn icon_pixmap(&self) -> Vec<Icon> {
            self.icon.clone()
        }

        fn tool_tip(&self) -> ToolTip {
            ToolTip {
                title: "Viper Mixer".into(),
                ..Default::default()
            }
        }

        fn menu(&self) -> Vec<MenuItem<Self>> {
            vec![
                entry("Show Viper Mixer", TrayEvent::Show),
                entry("Quit", TrayEvent::Quit),
            ]
        }
    }

    /// Spawn the StatusNotifierItem tray icon with Show and Quit menu items.
    ///
    /// # Arguments
    /// * `logo` - Image used for the tray icon.
    /// * `sender` - Receives menu selections.
    ///
    /// # Errors
    /// Returns a message if the desktop has no usable D-Bus session for the tray.
    pub(crate) fn create(
        logo: &DynamicImage,
        sender: Sender<TrayEvent>,
    ) -> Result<Handle<LinuxTray>, String> {
        LinuxTray {
            sender,
            icon: vec![argb_icon(logo)],
        }
        .spawn()
        .map_err(|error| error.to_string())
    }
}

#[cfg(not(target_os = "linux"))]
mod tray_icon_backend {
    use super::{DynamicImage, Sender, TrayEvent};
    use tray_icon::{
        menu::{Menu, MenuEvent, MenuItem},
        Icon, TrayIcon, TrayIconBuilder,
    };

    const TRAY_SHOW: &str = "show";
    const TRAY_QUIT: &str = "quit";

    /// Create the system tray icon with Show and Quit menu items.
    ///
    /// # Arguments
    /// * `logo` - Image used for the tray icon.
    /// * `sender` - Receives menu selections.
    ///
    /// # Errors
    /// Returns a message if the desktop has no usable system tray.
    pub(crate) fn create(
        logo: &DynamicImage,
        sender: Sender<TrayEvent>,
    ) -> Result<TrayIcon, String> {
        let menu = Menu::new();
        let show_item = MenuItem::with_id(TRAY_SHOW, "Show Viper Mixer", true, None);
        let quit_item = MenuItem::with_id(TRAY_QUIT, "Quit", true, None);
        menu.append_items(&[&show_item, &quit_item])
            .map_err(|error| error.to_string())?;

        let show_id = show_item.id().clone();
        let quit_id = quit_item.id().clone();
        MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
            if event.id == show_id {
                let _ = sender.send(TrayEvent::Show);
            } else if event.id == quit_id {
                let _ = sender.send(TrayEvent::Quit);
            }
        }));

        let icon_image = logo.thumbnail(96, 96).into_rgba8();
        let (width, height) = icon_image.dimensions();
        let icon = Icon::from_rgba(icon_image.into_raw(), width, height)
            .map_err(|error| error.to_string())?;

        TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_tooltip("Viper Mixer")
            .with_icon(icon)
            .build()
            .map_err(|error| error.to_string())
    }
}

#[cfg(target_os = "linux")]
pub(crate) use ksni_backend::create;
#[cfg(not(target_os = "linux"))]
pub(crate) use tray_icon_backend::create;
