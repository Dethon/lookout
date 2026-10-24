#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use lookout::{App, MainWindow};
use slint::ComponentHandle;

fn main() -> Result<(), slint::PlatformError> {
    lookout::tune_allocator();
    lookout::init_logging();

    let ui = MainWindow::new()?;
    let _app = match App::start(&ui) {
        Ok(app) => app,
        Err(error) => {
            log::error!("starting: {error}");
            return Err(slint::PlatformError::Other(error));
        }
    };
    ui.run()
}
