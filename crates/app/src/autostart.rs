//! Starting Lookout when the user signs in to Windows: a value under the
//! user's `Run` key, which is all the state there is, so Settings shows
//! what Windows will do.

/// Given by the `Run` value: Lookout starts in the notification area.
pub const HIDDEN: &str = "--hidden";

/// Started with `--hidden`, as the `Run` value starts it.
pub fn hidden() -> bool {
    std::env::args().skip(1).any(|arg| arg == HIDDEN)
}

#[cfg(windows)]
const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
#[cfg(windows)]
const NAME: &str = "Lookout";

#[cfg(windows)]
pub fn enabled() -> bool {
    crate::registry::get(RUN_KEY, NAME).is_some()
}

#[cfg(windows)]
pub fn set(on: bool) -> std::io::Result<()> {
    if on { crate::registry::set(RUN_KEY, NAME, &command()?) } else { crate::registry::delete(RUN_KEY, NAME) }
}

/// Points the `Run` value at this copy, should it have moved.
#[cfg(windows)]
pub fn refresh() {
    if let Some(value) = crate::registry::get(RUN_KEY, NAME)
        && let Ok(command) = command()
        && value != command
        && let Err(error) = crate::registry::set(RUN_KEY, NAME, &command)
    {
        log::warn!("start with Windows: {error}");
    }
}

#[cfg(windows)]
fn command() -> std::io::Result<String> {
    Ok(format!("\"{}\" {HIDDEN}", std::env::current_exe()?.display()))
}

#[cfg(not(windows))]
pub fn enabled() -> bool {
    false
}

#[cfg(not(windows))]
pub fn set(_on: bool) -> std::io::Result<()> {
    Err(std::io::ErrorKind::Unsupported.into())
}

#[cfg(not(windows))]
pub fn refresh() {}
