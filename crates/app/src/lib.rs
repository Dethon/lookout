//! Lookout's user interface.

mod app;
mod autostart;
pub mod body;
mod calendar_ui;
mod editor;
mod engine;
mod format;
mod instance;
mod logging;
mod mail_ui;
mod models;
mod notify;
mod page;
mod people_ui;
#[cfg(windows)]
mod registry;
mod repaint;
mod tray;
mod update_ui;
mod updater;
mod viewer;

slint::include_modules!();

pub use app::App;
pub use instance::claim as claim_instance;
pub use logging::init as init_logging;

/// Keeps glibc from growing a separate heap for every thread.
///
/// Its default allows eight heaps per core, and freed memory in one is not
/// reused by another: with a handful of mostly idle threads that left a
/// quarter of the heap unused. Other allocators need no tuning.
pub fn tune_allocator() {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    {
        const HEAPS: libc::c_int = 2;
        // SAFETY: `mallopt` only changes allocator parameters.
        unsafe { libc::mallopt(libc::M_ARENA_MAX, HEAPS) };
    }
}

/// Makes the standard widgets follow the theme.
pub fn set_color_scheme(ui: &MainWindow, dark: bool) {
    ui.global::<Palette>().set_color_scheme(if dark { slint::language::ColorScheme::Dark } else { slint::language::ColorScheme::Light });
}
