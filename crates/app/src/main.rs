#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use lookout::{App, MainWindow};

fn main() -> Result<(), slint::PlatformError> {
    lookout::tune_allocator();
    lookout::init_logging();
    if !lookout::claim_instance() {
        return Ok(());
    }

    let ui = MainWindow::new()?;
    let app = match App::start(&ui) {
        Ok(app) => app,
        Err(error) => {
            log::error!("starting: {error}");
            return Err(slint::PlatformError::Other(error));
        }
    };
    app.show()?;
    // Not when the window closes: it may be closed to the tray.
    let result = slint::run_event_loop_until_quit();
    app.quit();
    result
}
