//! One window at a time: a second launch brings the running one forward and
//! quits.
//!
//! The running instance listens on a Unix socket, or on Windows waits on a
//! named event; a second launch that reaches it signals it and stops. Both
//! are named after the data folder, so separate ones (`LOOKOUT_DATA_DIR`)
//! don't count as the same instance.

use slint::ComponentHandle;

/// Becomes the running instance, unless one already is: then it is asked to
/// come forward, and this returns false.
pub fn claim() -> bool {
    let Ok(dir) = lookout_core::config::data_dir() else { return true };
    // Something unexpected: better two windows than none.
    listen(&dir).unwrap_or(true)
}

/// A name for the data folder, short enough for a socket's path.
fn key(dir: &std::path::Path) -> String {
    use std::hash::{Hash, Hasher};

    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    dir.hash(&mut hasher);
    format!("lookout-{:016x}", hasher.finish())
}

/// Called on the listener's thread when a second launch asks.
fn asked() {
    _ = slint::invoke_from_event_loop(|| {
        crate::app::with_app(|app| {
            if let Some(ui) = app.ui() {
                bring_forward(&ui);
            }
        })
    });
}

fn bring_forward(ui: &crate::MainWindow) {
    use slint::winit_030::WinitWindowAccessor;

    let window = ui.window();
    _ = window.show();
    window.set_minimized(false);
    window.with_winit_window(|window| window.focus_window());
}

#[cfg(unix)]
fn socket(dir: &std::path::Path) -> std::path::PathBuf {
    dirs::runtime_dir().unwrap_or_else(std::env::temp_dir).join(format!("{}.sock", key(dir)))
}

#[cfg(unix)]
fn listen(dir: &std::path::Path) -> Option<bool> {
    use std::io::Write;
    use std::os::unix::net::{UnixListener, UnixStream};

    let path = socket(dir);
    if let Ok(mut running) = UnixStream::connect(&path) {
        _ = running.write_all(b"show");
        return Some(false);
    }
    // Left by an earlier instance: sockets outlive their listener.
    _ = std::fs::remove_file(&path);
    let listener = UnixListener::bind(&path).inspect_err(|error| log::warn!("single instance: {error}")).ok()?;
    std::thread::Builder::new()
        .name("instance".into())
        .spawn(move || {
            for _ in listener.incoming().flatten() {
                asked();
            }
        })
        .ok()?;
    Some(true)
}

#[cfg(windows)]
fn listen(dir: &std::path::Path) -> Option<bool> {
    use windows_sys::Win32::Foundation::{ERROR_ALREADY_EXISTS, GetLastError, HANDLE};
    use windows_sys::Win32::System::Threading::{CreateEventW, INFINITE, SetEvent, WaitForSingleObject};
    use windows_sys::Win32::UI::WindowsAndMessaging::{ASFW_ANY, AllowSetForegroundWindow};

    let name: Vec<u16> = format!("Local\\{}", key(dir)).encode_utf16().chain([0]).collect();
    // SAFETY: the name is NUL-terminated; default security, auto-reset.
    let event = unsafe { CreateEventW(std::ptr::null(), 0, 0, name.as_ptr()) };
    if event.is_null() {
        return None;
    }
    // SAFETY: read right after the call that set it.
    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        // SAFETY: plain calls on the event just opened. Windows only lets the
        // launched process take the foreground; this passes that on.
        unsafe {
            AllowSetForegroundWindow(ASFW_ANY);
            SetEvent(event);
        }
        return Some(false);
    }
    // Kept open for as long as the process runs.
    struct Event(HANDLE);
    // SAFETY: an event handle can be waited on from any thread.
    unsafe impl Send for Event {}
    let event = Event(event);
    std::thread::Builder::new()
        .name("instance".into())
        .spawn(move || {
            let event = event;
            // SAFETY: the handle stays open; the wait returns when signalled.
            while unsafe { WaitForSingleObject(event.0, INFINITE) } == 0 {
                asked();
            }
        })
        .ok()?;
    Some(true)
}

#[cfg(not(any(unix, windows)))]
fn listen(_dir: &std::path::Path) -> Option<bool> {
    None
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_second_claim_finds_the_first() {
        let dir = std::env::temp_dir().join(format!("lookout-instance-test-{}", std::process::id()));
        assert_eq!(super::listen(&dir), Some(true));
        assert_eq!(super::listen(&dir), Some(false));
        assert_eq!(super::listen(&dir.join("other")), Some(true), "another data folder is another instance");
        #[cfg(unix)]
        for dir in [dir.clone(), dir.join("other")] {
            _ = std::fs::remove_file(super::socket(&dir));
        }
    }
}
