//! The interface's side: owns the window's models, turns the user's clicks
//! into engine requests, and the engine's events into what is shown.

use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};
use std::time::Duration;

use lookout_core::{Account, Config, Store};
use slint::{ComponentHandle, ModelRc, SharedString, Timer, TimerMode, VecModel};

use crate::engine::{Engine, Event, Request};
use crate::{AccountRow, MainWindow, Page, Panes, Shell as AppGlobal, Theme};
use crate::{autostart, calendar_ui, mail_ui, notify, people_ui, tray, update_ui};

// How long a notice stays up.
const TOAST_TIME: Duration = Duration::from_secs(6);

thread_local! {
    static APP: RefCell<Weak<App>> = const { RefCell::new(Weak::new()) };
}

/// Runs `f` with the app, from a closure sent to the event loop.
pub fn with_app(f: impl FnOnce(&Rc<App>)) {
    APP.with(|app| {
        if let Some(app) = app.borrow().upgrade() {
            f(&app);
        }
    });
}

pub struct App {
    pub ui: slint::Weak<MainWindow>,
    pub engine: Engine,
    /// The interface's own reader of the cache.
    pub store: Rc<Store>,
    pub config: RefCell<Config>,
    pub accounts: RefCell<Vec<Account>>,
    pub mail: mail_ui::MailState,
    pub cal: calendar_ui::CalState,
    pub people: people_ui::PeopleState,
    pub update: update_ui::UpdateState,
    pub photos: Rc<crate::photos::Photos>,
    next_token: Cell<u64>,
    toast_timer: Timer,
    /// The notification area icon, while closing the window keeps Lookout
    /// running.
    tray: RefCell<Option<tray::Tray>>,
}

impl App {
    pub fn start(ui: &MainWindow) -> Result<Rc<Self>, String> {
        let config = Config::load();
        let path = Store::default_path().map_err(|error| error.to_string())?;
        let store = Rc::new(Store::open(&path).map_err(|error| error.to_string())?);
        let engine = Engine::start(path, config.clone(), |event| {
            _ = slint::invoke_from_event_loop(move || {
                APP.with(|app| {
                    if let Some(app) = app.borrow().upgrade() {
                        app.handle(event);
                    }
                });
            });
        });
        let photos = Rc::new(crate::photos::Photos::default());
        let app = Rc::new(Self {
            ui: ui.as_weak(),
            engine,
            mail: mail_ui::MailState::new(store.clone(), photos.clone()),
            cal: calendar_ui::CalState::new(),
            people: people_ui::PeopleState::default(),
            update: update_ui::UpdateState::default(),
            photos,
            store,
            config: RefCell::new(config),
            accounts: RefCell::new(Vec::new()),
            next_token: Cell::new(1),
            toast_timer: Timer::default(),
            tray: RefCell::new(None),
        });
        APP.with(|slot| *slot.borrow_mut() = Rc::downgrade(&app));
        app.wire(ui);
        crate::repaint::install(ui);
        app.set_tray(app.config.borrow().close_to_tray);
        autostart::refresh();
        Ok(app)
    }

    /// Shows the window, unless Windows started Lookout as the user signed
    /// in: then it waits in the tray, or minimized without one.
    pub fn show(&self) -> Result<(), slint::PlatformError> {
        let Some(ui) = self.ui() else { return Ok(()) };
        if !autostart::hidden() {
            return ui.show();
        }
        if self.tray.borrow().is_none() {
            ui.show()?;
            ui.window().set_minimized(true);
        }
        Ok(())
    }

    /// Takes the tray icon away as Lookout quits, rather than leave it
    /// there until the pointer passes over it.
    pub fn quit(&self) {
        self.tray.borrow_mut().take();
    }

    fn set_tray(&self, on: bool) {
        let mut tray = self.tray.borrow_mut();
        if on && tray.is_none() {
            *tray = tray::Tray::new();
        } else if !on {
            tray.take();
        }
    }

    /// Brings the window forward with a message open.
    pub fn show_message(self: &Rc<Self>, account: &str, folder: &str, id: &str) {
        let Some(ui) = self.ui() else { return };
        crate::instance::bring_forward(&ui);
        mail_ui::show_message(self, account, folder, id);
    }

    /// The user is looking at the window.
    fn in_front(&self) -> bool {
        use slint::winit_030::WinitWindowAccessor;

        let Some(ui) = self.ui() else { return false };
        ui.window().with_winit_window(|window| window.has_focus()).unwrap_or(false)
    }

    pub fn token(&self) -> u64 {
        let token = self.next_token.get();
        self.next_token.set(token + 1);
        token
    }

    pub fn ui(&self) -> Option<MainWindow> {
        self.ui.upgrade()
    }

    fn wire(self: &Rc<Self>, ui: &MainWindow) {
        let global = ui.global::<AppGlobal>();
        let config = self.config.borrow();
        ui.global::<Theme>().set_dark(config.dark);
        // Only an id the user set; empty means Lookout's own.
        global.set_client_id(config.client_id.clone().unwrap_or_default().into());
        global.set_load_images(config.load_remote_images);
        global.set_list_avatars(config.list_avatars);
        global.set_version(env!("CARGO_PKG_VERSION").into());
        global.set_status("Starting…".into());
        global.set_desktop(notify::AVAILABLE);
        global.set_close_to_tray(config.close_to_tray);
        global.set_notify_new_mail(config.notify_new_mail);
        global.set_start_with_windows(autostart::enabled());
        let panes = ui.global::<Panes>();
        panes.set_folders(config.panes.folders);
        panes.set_folders_collapsed(config.panes.folders_collapsed);
        panes.set_list(config.panes.list);
        panes.set_calendars_collapsed(config.panes.calendars_collapsed);
        panes.set_people(config.panes.people);
        drop(config);
        crate::set_color_scheme(ui, self.config.borrow().dark);

        let app = Rc::downgrade(self);
        global.on_add_account(move || {
            if let Some(app) = app.upgrade() {
                app.add_account();
            }
        });
        let app = Rc::downgrade(self);
        global.on_remove_account(move |id| {
            if let Some(app) = app.upgrade() {
                app.engine.send(Request::SignOut(id.into()));
            }
        });
        let app = Rc::downgrade(self);
        global.on_signature_edited(move |account, text| {
            if let Some(app) = app.upgrade() {
                app.config.borrow_mut().signatures.insert(account.into(), text.into());
                app.persist();
            }
        });
        let app = Rc::downgrade(self);
        global.on_sync_now(move || {
            if let Some(app) = app.upgrade() {
                app.engine.send(Request::SyncAll);
            }
        });
        let app = Rc::downgrade(self);
        global.on_save_settings(move || {
            if let Some(app) = app.upgrade() {
                app.save_settings();
            }
        });
        let app = Rc::downgrade(self);
        global.on_dark_changed(move |dark| {
            if let Some(app) = app.upgrade() {
                app.config.borrow_mut().dark = dark;
                if let Some(ui) = app.ui() {
                    ui.global::<Theme>().set_dark(dark);
                    crate::set_color_scheme(&ui, dark);
                }
                app.persist();
                mail_ui::theme_changed(&app);
            }
        });
        let app = Rc::downgrade(self);
        panes.on_changed(move || {
            if let Some(app) = app.upgrade()
                && let Some(ui) = app.ui()
            {
                let panes = ui.global::<Panes>();
                app.config.borrow_mut().panes = lookout_core::config::Panes {
                    folders: panes.get_folders(),
                    folders_collapsed: panes.get_folders_collapsed(),
                    list: panes.get_list(),
                    calendars_collapsed: panes.get_calendars_collapsed(),
                    people: panes.get_people(),
                };
                app.persist();
            }
        });
        let app = Rc::downgrade(self);
        global.on_list_avatars_changed(move |on| {
            if let Some(app) = app.upgrade() {
                app.config.borrow_mut().list_avatars = on;
                if let Some(ui) = app.ui() {
                    ui.global::<AppGlobal>().set_list_avatars(on);
                }
                app.persist();
            }
        });
        let app = Rc::downgrade(self);
        global.on_close_to_tray_changed(move |on| {
            if let Some(app) = app.upgrade() {
                app.config.borrow_mut().close_to_tray = on;
                app.persist();
                app.set_tray(on);
            }
        });
        let app = Rc::downgrade(self);
        global.on_notify_new_mail_changed(move |on| {
            if let Some(app) = app.upgrade() {
                app.config.borrow_mut().notify_new_mail = on;
                app.persist();
            }
        });
        let app = Rc::downgrade(self);
        global.on_start_with_windows_changed(move |on| {
            if let Some(app) = app.upgrade() {
                if let Err(error) = autostart::set(on) {
                    app.toast(&format!("Couldn't change starting with Windows: {error}"));
                }
                if let Some(ui) = app.ui() {
                    ui.global::<AppGlobal>().set_start_with_windows(autostart::enabled());
                }
            }
        });
        let app = Rc::downgrade(self);
        ui.window().on_close_requested(move || {
            // With no tray icon there would be no way back to the window.
            if !app.upgrade().is_some_and(|app| app.tray.borrow().is_some()) {
                _ = slint::quit_event_loop();
            }
            slint::CloseRequestResponse::HideWindow
        });
        let app = Rc::downgrade(self);
        global.on_dismiss_toast(move || {
            if let Some(app) = app.upgrade() {
                app.toast_timer.stop();
                if let Some(ui) = app.ui() {
                    ui.global::<AppGlobal>().set_toast("".into());
                }
            }
        });
        let app = Rc::downgrade(self);
        global.on_page_changed(move |page| {
            if let Some(app) = app.upgrade() {
                app.page_changed(page);
            }
        });
        let app = Rc::downgrade(self);
        global.on_key(move |key, control, shift| app.upgrade().is_some_and(|app| app.key(&key, control, shift)));

        mail_ui::wire(self, ui);
        calendar_ui::wire(self, ui);
        people_ui::wire(self, ui);
        update_ui::wire(self, ui);
    }

    fn add_account(&self) {
        let Some(ui) = self.ui() else { return };
        ui.global::<AppGlobal>().set_sign_in_error("".into());
        self.engine.send(Request::SignIn);
    }

    fn save_settings(&self) {
        let Some(ui) = self.ui() else { return };
        let global = ui.global::<AppGlobal>();
        {
            let mut config = self.config.borrow_mut();
            let client_id = global.get_client_id().trim().to_owned();
            config.client_id = (!client_id.is_empty()).then_some(client_id);
            config.load_remote_images = global.get_load_images();
        }
        self.persist();
        self.engine.send(Request::Configure(self.config.borrow().clone()));
    }

    pub fn persist(&self) {
        if let Err(error) = self.config.borrow().save() {
            self.status(&format!("Couldn't save settings: {error}"));
        }
    }

    pub fn status(&self, text: &str) {
        if let Some(ui) = self.ui() {
            ui.global::<AppGlobal>().set_status(text.into());
        }
    }

    pub fn toast(self: &Rc<Self>, text: &str) {
        let Some(ui) = self.ui() else { return };
        ui.global::<AppGlobal>().set_toast(text.into());
        let weak = self.ui.clone();
        self.toast_timer.start(TimerMode::SingleShot, TOAST_TIME, move || {
            if let Some(ui) = weak.upgrade() {
                ui.global::<AppGlobal>().set_toast("".into());
            }
        });
    }

    pub fn account_index(&self, account: &str) -> i32 {
        self.accounts.borrow().iter().position(|a| a.id == account).map(|i| i as i32).unwrap_or(0)
    }

    pub fn account_at(&self, index: i32) -> Option<Account> {
        let accounts = self.accounts.borrow();
        accounts.get(index.max(0) as usize).or(accounts.first()).cloned()
    }

    fn page_changed(self: &Rc<Self>, page: Page) {
        match page {
            Page::Calendar => calendar_ui::refresh(self),
            Page::People => people_ui::refresh(self),
            Page::Mail => {}
        }
    }

    fn key(self: &Rc<Self>, key: &str, control: bool, shift: bool) -> bool {
        let Some(ui) = self.ui() else { return false };
        let global = ui.global::<AppGlobal>();
        if control {
            let page = match key {
                "1" => Some(Page::Mail),
                "2" => Some(Page::Calendar),
                "3" => Some(Page::People),
                _ => None,
            };
            if let Some(page) = page {
                global.set_page(page);
                self.page_changed(page);
                return true;
            }
        }
        if key == "f5" {
            self.engine.send(Request::SyncAll);
            return true;
        }
        match global.get_page() {
            Page::Mail => mail_ui::key(self, key, control, shift),
            Page::Calendar => calendar_ui::key(self, key, control, shift),
            Page::People => false,
        }
    }

    fn handle(self: &Rc<Self>, event: Event) {
        let Some(ui) = self.ui() else { return };
        let global = ui.global::<AppGlobal>();
        match event {
            Event::Accounts(accounts) => {
                let rows: Vec<AccountRow> = accounts
                    .iter()
                    .map(|account| AccountRow {
                        id: account.id.as_str().into(),
                        name: account.name.as_str().into(),
                        email: account.email.as_str().into(),
                        signature: self.config.borrow().signatures.get(&account.id).cloned().unwrap_or_default().into(),
                    })
                    .collect();
                let labels: Vec<SharedString> = accounts
                    .iter()
                    .map(|account| format!("{} <{}>", account.name, account.email).into())
                    .collect();
                global.set_accounts(ModelRc::new(VecModel::from(rows)));
                global.set_account_labels(ModelRc::new(VecModel::from(labels)));
                *self.accounts.borrow_mut() = accounts;
                mail_ui::accounts_changed(self);
                if self.accounts.borrow().is_empty() {
                    global.set_status("".into());
                }
            }
            Event::SigningIn(busy) => global.set_signing_in(busy),
            Event::SignInFailed(error) => {
                global.set_signing_in(false);
                global.set_sign_in_error(error.as_str().into());
                self.toast(&format!("Sign-in failed: {error}"));
            }
            Event::Syncing(busy) => global.set_syncing(busy),
            Event::Status(text) => global.set_status(text.into()),
            Event::Error(text) => {
                global.set_status(text.as_str().into());
                self.toast(&text);
            }
            Event::NewMail(messages) => {
                let text = match messages.as_slice() {
                    [one] => format!("{}: {}", one.from.display(), one.subject),
                    many => format!("{} new messages", many.len()),
                };
                self.toast(&text);
                if self.config.borrow().notify_new_mail && !self.in_front() {
                    notify::new_mail(&messages);
                }
            }
            Event::Folders => mail_ui::folders_changed(self),
            Event::Messages { account, folder } => mail_ui::messages_changed(self, &account, &folder),
            Event::Opened { token, detail } => mail_ui::opened(self, token, detail),
            Event::Pictures { token, pictures } => mail_ui::pictures(self, token, pictures),
            Event::OriginalReady { token, path } => mail_ui::original_ready(self, token, path),
            Event::ReplyReady { token, draft } => mail_ui::reply_ready(self, token, draft),
            Event::Sent => mail_ui::sent(self),
            Event::SendFailed(error) => mail_ui::send_failed(self, &error),
            Event::SearchResults { token, ids } => mail_ui::search_results(self, token, ids),
            Event::Calendar => calendar_ui::refresh(self),
            Event::CalendarBody { account, id, html } => calendar_ui::event_body(self, &account, &id, &html),
            Event::CalendarSaved => calendar_ui::saved(self),
            Event::CalendarFailed(error) => calendar_ui::failed(self, &error),
            Event::Contacts => {
                self.photos.clear();
                people_ui::refresh(self);
                mail_ui::contacts_changed(self);
            }
            Event::Photos => {
                self.photos.clear();
                people_ui::refresh(self);
                mail_ui::photos_changed(self);
            }
            Event::ContactSaved => people_ui::saved(self),
            Event::ContactFailed(error) => people_ui::failed(self, &error),
        }
    }
}
