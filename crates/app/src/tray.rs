//! Lookout's icon in the notification area, on Windows: closing the window
//! leaves it running there, a click on the icon brings it back, and its
//! menu quits.

#[cfg(windows)]
pub use imp::Tray;

/// No notification area elsewhere: closing the window quits.
#[cfg(not(windows))]
pub struct Tray;

#[cfg(not(windows))]
impl Tray {
    pub fn new() -> Option<Self> {
        None
    }
}

#[cfg(windows)]
mod imp {
    use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
    use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

    // The icon group `build.rs` embeds in the executable.
    const ICON_RESOURCE: u16 = 1;
    const OPEN: &str = "open";
    const QUIT: &str = "quit";

    /// The icon; dropping it takes it out of the notification area.
    pub struct Tray(#[allow(dead_code)] TrayIcon);

    impl Tray {
        pub fn new() -> Option<Self> {
            static HANDLERS: std::sync::Once = std::sync::Once::new();
            // Kept for the whole run: the icon's handlers can be set once.
            HANDLERS.call_once(|| {
                TrayIconEvent::set_event_handler(Some(clicked));
                MenuEvent::set_event_handler(Some(chosen));
            });
            let menu = Menu::new();
            let built = menu.append_items(&[
                &MenuItem::with_id(OPEN, "Open Lookout", true, None),
                &PredefinedMenuItem::separator(),
                &MenuItem::with_id(QUIT, "Quit", true, None),
            ]);
            if let Err(error) = built {
                log::warn!("tray menu: {error}");
                return None;
            }
            let icon = Icon::from_resource(ICON_RESOURCE, None).inspect_err(|error| log::warn!("tray icon: {error}")).ok()?;
            TrayIconBuilder::new()
                .with_icon(icon)
                .with_tooltip("Lookout")
                .with_menu(Box::new(menu))
                .with_menu_on_left_click(false)
                .build()
                .inspect_err(|error| log::warn!("tray icon: {error}"))
                .ok()
                .map(Self)
        }
    }

    // Both handlers run inside the window's message handling: the work
    // waits for the event loop.

    fn clicked(event: TrayIconEvent) {
        if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = event {
            show();
        }
    }

    fn chosen(event: MenuEvent) {
        match event.id().as_ref() {
            OPEN => show(),
            QUIT => _ = slint::invoke_from_event_loop(|| _ = slint::quit_event_loop()),
            _ => {}
        }
    }

    fn show() {
        _ = slint::invoke_from_event_loop(|| {
            crate::app::with_app(|app| {
                if let Some(ui) = app.ui() {
                    crate::instance::bring_forward(&ui);
                }
            })
        });
    }
}
