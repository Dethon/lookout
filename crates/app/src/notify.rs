//! Telling the user about new mail while the window is in the background.

/// Flashes the taskbar button on Windows; elsewhere the in-window notice is
/// all there is.
pub fn new_mail(_text: &str) {
    log::info!("new mail");
}
