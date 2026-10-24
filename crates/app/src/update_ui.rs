//! New versions: the banner, the Settings rows, and when to look.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, SystemTime};

use slint::{ComponentHandle, Timer, TimerMode};

use crate::app::App;
use crate::updater::{self, Available, Found};
use crate::{AppUpdate, MainWindow};

// The first look for a new version waits until starting up is done.
const CHECK_DELAY: Duration = Duration::from_secs(10);
// How often the clock is read to see whether a look is due: a timer of a
// whole day would not count the time the computer sleeps.
const CHECK_TICK: Duration = Duration::from_secs(60 * 60);
const CHECK_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);

#[derive(Default)]
pub struct UpdateState {
    timer: Timer,
    /// The newer release found, if any.
    available: RefCell<Option<Available>>,
    /// The version whose banner was shown: closing it keeps it closed.
    seen: RefCell<Option<String>>,
    /// When the last look succeeded.
    checked: Cell<Option<SystemTime>>,
}

pub fn wire(app: &Rc<App>, ui: &MainWindow) {
    let update = ui.global::<AppUpdate>();
    update.set_automatic(app.config.borrow().check_for_updates);
    let weak = Rc::downgrade(app);
    update.on_check(move || {
        if let Some(app) = weak.upgrade() {
            check(&app, true);
        }
    });
    let weak = Rc::downgrade(app);
    update.on_install(move || {
        if let Some(app) = weak.upgrade() {
            install(&app);
        }
    });
    let weak = Rc::downgrade(app);
    update.on_open_page(move || {
        if let Some(app) = weak.upgrade() {
            open_page(&app);
        }
    });
    let weak = Rc::downgrade(app);
    update.on_dismiss(move || {
        if let Some(app) = weak.upgrade() {
            let version = app.update.available.borrow().as_ref().map(|available| available.version.clone());
            *app.update.seen.borrow_mut() = version;
            if let Some(ui) = app.ui() {
                ui.global::<AppUpdate>().set_banner(false);
            }
        }
    });
    let weak = Rc::downgrade(app);
    update.on_automatic_changed(move |on| {
        if let Some(app) = weak.upgrade() {
            app.config.borrow_mut().check_for_updates = on;
            app.persist();
            check_if_due(&app);
        }
    });
    watch(app);
}

/// Looks for a new version shortly after starting and then once a day,
/// while the setting is on.
fn watch(app: &Rc<App>) {
    updater::clean_up();
    let weak = Rc::downgrade(app);
    Timer::single_shot(CHECK_DELAY, move || {
        if let Some(app) = weak.upgrade() {
            check_if_due(&app);
        }
    });
    let weak = Rc::downgrade(app);
    app.update.timer.start(TimerMode::Repeated, CHECK_TICK, move || {
        if let Some(app) = weak.upgrade() {
            check_if_due(&app);
        }
    });
}

fn check_if_due(app: &Rc<App>) {
    if !app.config.borrow().check_for_updates {
        return;
    }
    // A clock set back counts as due.
    let due = app
        .update
        .checked
        .get()
        .is_none_or(|checked| !checked.elapsed().is_ok_and(|elapsed| elapsed < CHECK_INTERVAL));
    if due {
        check(app, false);
    }
}

/// Looks for a new version; `asked` when the user did, who then hears
/// about a failure too.
fn check(app: &Rc<App>, asked: bool) {
    let started = updater::check(move |found| {
        _ = slint::invoke_from_event_loop(move || crate::app::with_app(|app| found_release(app, found, asked)));
    });
    if started && let Some(ui) = app.ui() {
        ui.global::<AppUpdate>().set_checking(true);
    }
}

fn found_release(app: &Rc<App>, found: Found, asked: bool) {
    let state = &app.update;
    let status = match found {
        Found::UpToDate => {
            state.checked.set(Some(SystemTime::now()));
            state.available.borrow_mut().take();
            Some("Lookout is up to date.".to_owned())
        }
        Found::Newer(available) => {
            state.checked.set(Some(SystemTime::now()));
            let status = if available.installer.is_some() {
                format!("Lookout {} is ready to install.", available.version)
            } else {
                format!("Lookout {} is available.", available.version)
            };
            *state.available.borrow_mut() = Some(available);
            Some(status)
        }
        // Automatic looks fail quietly and try again within the hour.
        Found::Failed(error) => asked.then(|| format!("Couldn't check for updates: {error}")),
    };
    let available = state.available.borrow().clone();
    let new = available.as_ref().is_some_and(|available| state.seen.borrow().as_deref() != Some(&available.version));
    if let Some(ui) = app.ui() {
        let update = ui.global::<AppUpdate>();
        update.set_checking(false);
        if let Some(status) = status {
            update.set_status(status.into());
        }
        update.set_version(available.as_ref().map(|available| available.version.as_str()).unwrap_or_default().into());
        update.set_ready(available.as_ref().is_some_and(|available| available.installer.is_some()));
        update.set_banner(new || (update.get_banner() && available.is_some()));
    }
    // Shown once; closing the banner keeps it closed for this version.
    if new {
        *state.seen.borrow_mut() = available.map(|available| available.version);
    }
}

/// Runs the downloaded installer and quits, so it can replace this copy;
/// it opens the new version when it is done.
fn install(app: &Rc<App>) {
    let installer = app.update.available.borrow().as_ref().and_then(|available| available.installer.clone());
    let Some(installer) = installer else { return };
    match updater::install(&installer) {
        Ok(()) => {
            log::info!("installing {}", installer.display());
            _ = slint::quit_event_loop();
        }
        Err(error) => {
            log::warn!("couldn't start {}: {error}", installer.display());
            app.toast(&format!("Couldn't start the update: {error}"));
        }
    }
}

fn open_page(app: &Rc<App>) {
    let page = app.update.available.borrow().as_ref().map(|available| available.page.clone());
    let page = page.unwrap_or_else(|| lookout_core::release::CHANGELOG.to_owned());
    if let Err(error) = open::that_detached(&page) {
        log::warn!("couldn't open {page}: {error}");
    }
}
