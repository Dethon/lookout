//! The engine's thread: owns the accounts' connections and the cache's
//! writer, syncs on a timer, and answers the interface's requests.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use lookout_core::calendar::{Answer, EventDraft};
use lookout_core::mail::{Draft, ReplyKind};
use lookout_core::model::{Attachment, Contact, Flag, MessageDetail, MessageSummary, wire};
use lookout_core::{Account, Config, Error, Graph, Session, Store, auth, calendar, graph, mail, people, sync};
use slint::{Rgba8Pixel, SharedPixelBuffer};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

const WORKER_THREADS: usize = 2;
const CALENDAR_EVERY: Duration = Duration::from_secs(5 * 60);
const CONTACTS_EVERY: Duration = Duration::from_secs(30 * 60);
// The window of events kept in the cache around today.
const CALENDAR_BEFORE: i64 = 60 * 86400;
const CALENDAR_AFTER: i64 = 365 * 86400;
// Pictures in messages are scaled down to this width; wider would only be
// shrunk on screen while taking memory.
const MAX_PICTURE_WIDTH: u32 = 900;
const MAX_PICTURE_HEIGHT: u32 = 3000;
// The same for messages drawn by Blitz, which keeps pictures decoded at
// their own size: room for a wide pane on a high-resolution screen.
const MAX_PAGE_PICTURE_WIDTH: u32 = 1800;
const MAX_PAGE_PICTURE_HEIGHT: u32 = 3600;
// Folders kept up to date even when not open.
const ALWAYS_WATCHED: &[&str] = &["inbox", "drafts", "sentitems"];

pub enum Request {
    SignIn,
    SignOut(String),
    SyncAll,
    /// Keeps a folder up to date from now on, and syncs it now.
    Watch { account: String, folder: String },
    Open { account: String, id: String, token: u64 },
    SetRead { account: String, id: String, read: bool },
    SetFlag { account: String, id: String, flag: Flag },
    Move { account: String, id: String, folder: String },
    Delete { account: String, id: String },
    /// One change to several messages of an account.
    Bulk { account: String, ids: Vec<String>, change: mail::Bulk },
    StartReply { account: String, id: String, kind: ReplyKind, token: u64 },
    Send { account: String, draft: Box<Draft>, reply: Option<Box<wire::Message>>, existing: Option<String> },
    SaveDraft { account: String, draft: Draft },
    Attachment { account: String, message: String, attachment: String, name: String, open: bool },
    Pictures { account: String, message: String, token: u64, sources: Vec<String>, remote: bool },
    /// A picture, style sheet or font of a message drawn by Blitz; `reply`
    /// gets its bytes, or `None` when it can't or mustn't be loaded.
    Resource { account: String, message: String, url: String, remote: bool, reply: Box<dyn FnOnce(Option<Vec<u8>>) + Send> },
    /// Writes the message as it was sent to a page; see `viewer.rs`.
    PrepareOriginal { account: String, id: String, remote: bool, token: u64 },
    SearchServer { query: String, token: u64 },
    /// The calendar now shows `[start, end)`.
    CalendarRange { start: i64, end: i64 },
    EventBody { account: String, id: String },
    SaveEvent { account: String, id: Option<String>, draft: EventDraft },
    DeleteEvent { account: String, id: String, cancel: bool },
    Respond { account: String, id: String, answer: Answer },
    SaveContact { contact: Contact },
    DeleteContact { account: String, id: String },
    Configure(Config),
}

pub enum Event {
    Accounts(Vec<Account>),
    SigningIn(bool),
    SignInFailed(String),
    Folders,
    Messages { account: String, folder: String },
    Opened { token: u64, detail: Option<MessageDetail> },
    ReplyReady { token: u64, draft: wire::Message },
    Sent,
    SendFailed(String),
    Pictures { token: u64, pictures: Vec<(String, SharedPixelBuffer<Rgba8Pixel>)> },
    OriginalReady { token: u64, path: PathBuf },
    SearchResults { token: u64, ids: Vec<(String, String)> },
    Calendar,
    CalendarBody { account: String, id: String, html: String },
    CalendarSaved,
    CalendarFailed(String),
    Contacts,
    ContactSaved,
    ContactFailed(String),
    NewMail(Vec<MessageSummary>),
    Syncing(bool),
    Status(String),
    Error(String),
}

type Notify = Arc<dyn Fn(Event) + Send + Sync>;

struct Inner {
    store: Arc<Mutex<Store>>,
    http: lookout_core::reqwest::Client,
    config: Mutex<Config>,
    accounts: Mutex<HashMap<String, Graph>>,
    watched: Mutex<HashMap<String, HashSet<String>>>,
    /// One message sync at a time per account, so delta pages aren't
    /// applied twice.
    sync_locks: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    calendar_window: Mutex<(i64, i64)>,
    last_calendar: Mutex<Option<Instant>>,
    last_contacts: Mutex<Option<Instant>>,
    notify: Notify,
}

#[derive(Clone)]
pub struct Engine {
    requests: UnboundedSender<Request>,
}

impl Engine {
    pub fn start(store_path: PathBuf, config: Config, notify: impl Fn(Event) + Send + Sync + 'static) -> Self {
        let (requests, receiver) = unbounded_channel();
        let notify: Notify = Arc::new(notify);
        std::thread::Builder::new()
            .name("engine".into())
            .spawn(move || {
                let runtime = tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(WORKER_THREADS)
                    .max_blocking_threads(2)
                    .thread_keep_alive(Duration::from_secs(2))
                    .enable_all()
                    .build()
                    .expect("the runtime starts");
                runtime.block_on(run(store_path, config, notify, receiver));
            })
            .expect("the engine thread starts");
        Self { requests }
    }

    pub fn send(&self, request: Request) {
        _ = self.requests.send(request);
    }

    /// An engine that drops every request.
    #[cfg(test)]
    pub fn idle() -> Self {
        Self { requests: unbounded_channel().0 }
    }
}

async fn run(store_path: PathBuf, config: Config, notify: Notify, mut receiver: UnboundedReceiver<Request>) {
    let store = match Store::open(&store_path) {
        Ok(store) => store,
        Err(error) => {
            notify(Event::Error(format!("can't open the cache: {error}")));
            return;
        }
    };
    let now = now();
    let inner = Arc::new(Inner {
        store: Arc::new(Mutex::new(store)),
        http: graph::http_client(),
        config: Mutex::new(config),
        accounts: Mutex::new(HashMap::new()),
        watched: Mutex::new(HashMap::new()),
        sync_locks: Mutex::new(HashMap::new()),
        calendar_window: Mutex::new((now - CALENDAR_BEFORE, now + CALENDAR_AFTER)),
        last_calendar: Mutex::new(None),
        last_contacts: Mutex::new(None),
        notify,
    });
    inner.restore_accounts();
    tokio::spawn(inner.clone().sync_all());

    let mut poll_started = Instant::now();
    loop {
        let poll = Duration::from_secs(u64::from(inner.config.lock().unwrap().poll_seconds.max(15)));
        let wait = poll.saturating_sub(poll_started.elapsed());
        tokio::select! {
            request = receiver.recv() => match request {
                Some(request) => {
                    let inner = inner.clone();
                    tokio::spawn(async move { inner.handle(request).await });
                }
                None => break,
            },
            () = tokio::time::sleep(wait) => {
                poll_started = Instant::now();
                tokio::spawn(inner.clone().sync_all());
            }
        }
    }
}

fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

fn message_of(error: &Error) -> String {
    match error {
        Error::Graph { message, .. } if !message.is_empty() => message.clone(),
        other => other.to_string(),
    }
}

impl Inner {
    fn emit(&self, event: Event) {
        (self.notify)(event);
    }

    fn graph(&self, account: &str) -> Option<Graph> {
        self.accounts.lock().unwrap().get(account).cloned()
    }

    fn graphs(&self) -> Vec<Graph> {
        self.accounts.lock().unwrap().values().cloned().collect()
    }

    fn store_accounts(&self) -> Vec<Account> {
        self.store.lock().unwrap().accounts().unwrap_or_default()
    }

    fn restore_accounts(&self) {
        let client_id = match self.config.lock().unwrap().client_id() {
            Ok(id) => id,
            Err(_) => {
                self.emit(Event::Accounts(self.store_accounts()));
                return;
            }
        };
        let accounts = self.store_accounts();
        let mut connected = self.accounts.lock().unwrap();
        for account in &accounts {
            match Session::restore(self.http.clone(), &client_id, &account.id) {
                Some(session) => {
                    connected.insert(account.id.clone(), Graph::new(self.http.clone(), Arc::new(session)));
                }
                None => log::warn!("no saved sign-in for {}", account.email),
            }
        }
        drop(connected);
        self.emit(Event::Accounts(accounts));
    }

    fn sync_lock(&self, account: &str) -> Arc<tokio::sync::Mutex<()>> {
        self.sync_locks.lock().unwrap().entry(account.to_owned()).or_default().clone()
    }

    fn report(&self, what: &str, error: &Error) {
        log::warn!("{what}: {error}");
        if matches!(error, Error::SignInRequired) {
            self.emit(Event::Error(format!("{what}: sign in again from Settings")));
        } else {
            self.emit(Event::Status(format!("{what}: {}", message_of(error))));
        }
    }

    async fn sync_all(self: Arc<Self>) {
        let graphs = self.graphs();
        if graphs.is_empty() {
            return;
        }
        self.emit(Event::Syncing(true));
        for graph in &graphs {
            self.sync_mail(graph).await;
        }
        let calendar_due = self.last_calendar.lock().unwrap().is_none_or(|last| last.elapsed() >= CALENDAR_EVERY);
        if calendar_due {
            *self.last_calendar.lock().unwrap() = Some(Instant::now());
            let (start, end) = *self.calendar_window.lock().unwrap();
            for graph in &graphs {
                self.sync_calendar(graph, start, end).await;
            }
            self.emit(Event::Calendar);
        }
        let contacts_due = self.last_contacts.lock().unwrap().is_none_or(|last| last.elapsed() >= CONTACTS_EVERY);
        if contacts_due {
            *self.last_contacts.lock().unwrap() = Some(Instant::now());
            for graph in &graphs {
                if let Err(error) = sync::sync_contacts(graph, &self.store).await {
                    self.report("Updating contacts", &error);
                }
            }
            self.emit(Event::Contacts);
        }
        self.emit(Event::Syncing(false));
        self.emit(Event::Status(format!("Up to date at {}", chrono::Local::now().format("%H:%M"))));
    }

    async fn sync_mail(&self, graph: &Graph) {
        let account = graph.account_id().to_owned();
        let folders = match sync::sync_folders(graph, &self.store).await {
            Ok(folders) => folders,
            Err(error) => {
                self.report("Updating folders", &error);
                return;
            }
        };
        self.emit(Event::Folders);
        let mut targets: Vec<String> = folders
            .iter()
            .filter(|folder| folder.well_known.as_deref().is_some_and(|name| ALWAYS_WATCHED.contains(&name)))
            .map(|folder| folder.id.clone())
            .collect();
        if let Some(watched) = self.watched.lock().unwrap().get(&account) {
            targets.extend(watched.iter().filter(|id| !targets.contains(id)).cloned().collect::<Vec<_>>());
        }
        let inbox = folders.iter().find(|f| f.well_known.as_deref() == Some("inbox")).map(|f| f.id.clone());
        for folder in targets {
            if !folders.iter().any(|f| f.id == folder) {
                continue;
            }
            self.sync_folder(graph, &folder, inbox.as_deref() == Some(folder.as_str())).await;
        }
    }

    async fn sync_folder(&self, graph: &Graph, folder: &str, notify_new: bool) {
        let account = graph.account_id().to_owned();
        let lock = self.sync_lock(&account);
        let _guard = lock.lock().await;
        let mut pages = 0u32;
        let result = sync::sync_messages(graph, &self.store, folder, || {
            pages += 1;
            // Refresh the list as the first pages come in, then less often.
            if pages <= 2 || pages.is_multiple_of(10) {
                self.emit(Event::Messages { account: account.clone(), folder: folder.to_owned() });
            }
        })
        .await;
        match result {
            Ok(result) => {
                if result.changed {
                    self.emit(Event::Messages { account: account.clone(), folder: folder.to_owned() });
                    self.emit(Event::Folders);
                }
                if notify_new && !result.arrived.is_empty() {
                    self.emit(Event::NewMail(result.arrived));
                }
            }
            Err(error) => self.report("Updating messages", &error),
        }
    }

    async fn sync_calendar(&self, graph: &Graph, start: i64, end: i64) {
        if let Err(error) = sync::sync_time_zone(graph, &self.store).await {
            log::info!("no mailbox time zone: {error}");
        }
        let calendars = match sync::sync_calendars(graph, &self.store).await {
            Ok(calendars) => calendars,
            Err(error) => {
                self.report("Updating calendars", &error);
                return;
            }
        };
        for calendar in calendars {
            if let Err(error) = sync::sync_events(graph, &self.store, &calendar.id, start, end).await {
                self.report(&format!("Updating {}", calendar.name), &error);
            }
        }
    }

    async fn handle(self: Arc<Self>, request: Request) {
        // For looking at a real mailbox without changing it (see examples/live.rs).
        let changes = matches!(
            request,
            Request::SetRead { .. }
                | Request::SetFlag { .. }
                | Request::Move { .. }
                | Request::Delete { .. }
                | Request::Bulk { .. }
                | Request::Send { .. }
                | Request::SaveDraft { .. }
                | Request::SaveEvent { .. }
                | Request::DeleteEvent { .. }
                | Request::Respond { .. }
                | Request::SaveContact { .. }
                | Request::DeleteContact { .. }
        );
        if changes && std::env::var_os("LOOKOUT_READ_ONLY").is_some() {
            log::info!("read-only: a change was not made");
            return;
        }
        match request {
            Request::SignIn => self.sign_in().await,
            Request::SignOut(account) => {
                auth::sign_out(&account);
                self.accounts.lock().unwrap().remove(&account);
                if let Err(error) = self.store.lock().unwrap().remove_account(&account) {
                    log::warn!("removing {account}: {error}");
                }
                self.emit(Event::Accounts(self.store_accounts()));
                self.emit(Event::Folders);
                self.emit(Event::Calendar);
                self.emit(Event::Contacts);
            }
            Request::SyncAll => {
                *self.last_calendar.lock().unwrap() = None;
                *self.last_contacts.lock().unwrap() = None;
                self.sync_all().await;
            }
            Request::Watch { account, folder } => {
                self.watched.lock().unwrap().entry(account.clone()).or_default().insert(folder.clone());
                if let Some(graph) = self.graph(&account) {
                    self.sync_folder(&graph, &folder, false).await;
                }
            }
            Request::Open { account, id, token } => {
                let Some(graph) = self.graph(&account) else {
                    // Signed out or offline: what the cache has.
                    let detail = self.store.lock().unwrap().message(&account, &id).ok().flatten();
                    self.emit(Event::Opened { token, detail });
                    return;
                };
                match mail::open(&graph, &self.store, &id).await {
                    Ok(detail) => self.emit(Event::Opened { token, detail }),
                    Err(error) => {
                        self.report("Opening the message", &error);
                        // Show what the cache has, without the body.
                        let detail = self.store.lock().unwrap().message(&account, &id).ok().flatten();
                        self.emit(Event::Opened { token, detail });
                    }
                }
            }
            Request::SetRead { account, id, read } => {
                let Some(graph) = self.graph(&account) else { return };
                let result = mail::set_read(&graph, &self.store, &id, read, || self.changed_message(&account, &id)).await;
                if let Err(error) = result {
                    self.report("Marking the message", &error);
                }
            }
            Request::SetFlag { account, id, flag } => {
                let Some(graph) = self.graph(&account) else { return };
                let result = mail::set_flag(&graph, &self.store, &id, flag, || self.changed_message(&account, &id)).await;
                if let Err(error) = result {
                    self.report("Flagging the message", &error);
                }
            }
            Request::Move { account, id, folder } => {
                let Some(graph) = self.graph(&account) else { return };
                let source = self.folder_of(&account, &id);
                let result = mail::move_to(&graph, &self.store, &id, &folder, || self.after_move(&account, source)).await;
                if let Err(error) = result {
                    self.report("Moving the message", &error);
                }
            }
            Request::Delete { account, id } => {
                let Some(graph) = self.graph(&account) else { return };
                let source = self.folder_of(&account, &id);
                let result = mail::delete(&graph, &self.store, &id, || self.after_move(&account, source)).await;
                if let Err(error) = result {
                    self.report("Deleting the message", &error);
                }
            }
            Request::Bulk { account, ids, change } => {
                let Some(graph) = self.graph(&account) else { return };
                let sources: HashSet<String> = ids.iter().filter_map(|id| self.folder_of(&account, id)).collect();
                let refresh = || {
                    for folder in &sources {
                        self.emit(Event::Messages { account: account.clone(), folder: folder.clone() });
                    }
                    self.emit(Event::Folders);
                };
                if let Err(error) = mail::bulk(&graph, &self.store, &ids, &change, refresh).await {
                    self.report("Changing the messages", &error);
                }
            }
            Request::StartReply { account, id, kind, token } => {
                let Some(graph) = self.graph(&account) else { return };
                match mail::start_reply(&graph, kind, &id).await {
                    Ok(draft) => self.emit(Event::ReplyReady { token, draft }),
                    Err(error) => self.report("Starting the reply", &error),
                }
            }
            Request::Send { account, draft, reply, existing } => {
                let Some(graph) = self.graph(&account) else { return };
                self.emit(Event::Status("Sending…".into()));
                let result = match existing {
                    Some(id) => mail::send_draft(&graph, &id, &draft).await,
                    None => mail::send(&graph, &draft, reply.as_deref()).await,
                };
                match result {
                    Ok(()) => {
                        self.emit(Event::Sent);
                        self.emit(Event::Status("Sent".into()));
                        // The copy in Sent Items, and the reply's state.
                        tokio::time::sleep(Duration::from_secs(3)).await;
                        self.sync_mail(&graph).await;
                    }
                    Err(error) => {
                        log::warn!("sending: {error}");
                        self.emit(Event::SendFailed(message_of(&error)));
                    }
                }
            }
            Request::SaveDraft { account, draft } => {
                let Some(graph) = self.graph(&account) else { return };
                match mail::save_draft(&graph, &draft).await {
                    Ok(_) => {
                        self.emit(Event::Status("Draft saved".into()));
                        self.sync_mail(&graph).await;
                    }
                    Err(error) => self.report("Saving the draft", &error),
                }
            }
            Request::Attachment { account, message, attachment, name, open } => {
                let Some(graph) = self.graph(&account) else { return };
                self.emit(Event::Status(format!("Downloading {name}…")));
                match mail::attachment_bytes(&graph, &message, &attachment).await {
                    Ok(bytes) => match save_download(&name, &bytes, open) {
                        Ok(path) => {
                            if open {
                                if let Err(error) = open::that_detached(&path) {
                                    self.emit(Event::Status(format!("Can't open {name}: {error}")));
                                    return;
                                }
                                self.emit(Event::Status(format!("Opened {name}")));
                            } else {
                                self.emit(Event::Status(format!("Saved to {}", path.display())));
                            }
                        }
                        Err(error) => self.emit(Event::Status(format!("Can't save {name}: {error}"))),
                    },
                    Err(error) => self.report("Downloading the attachment", &error),
                }
            }
            Request::Pictures { account, message, token, sources, remote } => {
                let Some(graph) = self.graph(&account) else { return };
                let pictures = self.pictures(&graph, &message, &sources, remote).await;
                if !pictures.is_empty() {
                    self.emit(Event::Pictures { token, pictures });
                }
            }
            Request::Resource { account, message, url, remote, reply } => {
                let Some(graph) = self.graph(&account) else { return reply(None) };
                let attachments = self.attachments(&graph, &message);
                let bytes = match self.resource(&graph, &message, &attachments, &url, remote).await {
                    Some(bytes) => tokio::task::spawn_blocking(move || shrink_picture(bytes)).await.ok(),
                    None => None,
                };
                reply(bytes);
            }
            Request::PrepareOriginal { account, id, remote, token } => {
                let Some(graph) = self.graph(&account) else { return };
                match self.prepare_original(&graph, &id, remote).await {
                    Ok(path) => self.emit(Event::OriginalReady { token, path }),
                    Err(error) => self.emit(Event::Status(format!("Can't show the original: {error}"))),
                }
            }
            Request::SearchServer { query, token } => {
                let mut ids = Vec::new();
                for graph in self.graphs() {
                    match mail::search_server(&graph, &self.store, &query).await {
                        Ok(found) => ids.extend(found.into_iter().map(|id| (graph.account_id().to_owned(), id))),
                        Err(error) => self.report("Searching", &error),
                    }
                }
                self.emit(Event::SearchResults { token, ids });
            }
            Request::CalendarRange { start, end } => {
                let (window_start, window_end) = *self.calendar_window.lock().unwrap();
                if start >= window_start && end <= window_end {
                    return;
                }
                // Outside the kept window: fetch it, and widen the window.
                {
                    let mut window = self.calendar_window.lock().unwrap();
                    *window = (window.0.min(start), window.1.max(end));
                }
                for graph in self.graphs() {
                    self.sync_calendar(&graph, start, end).await;
                }
                self.emit(Event::Calendar);
            }
            Request::EventBody { account, id } => {
                let Some(graph) = self.graph(&account) else { return };
                match calendar::body(&graph, &self.store, &id).await {
                    Ok(html) => self.emit(Event::CalendarBody { account, id, html }),
                    Err(error) => self.report("Opening the event", &error),
                }
            }
            Request::SaveEvent { account, id, mut draft } => {
                let Some(graph) = self.graph(&account) else { return };
                draft.time_zone =
                    self.store.lock().unwrap().sync_link(&account, sync::TIME_ZONE_KEY).ok().flatten().unwrap_or_default();
                let result = match &id {
                    Some(id) => calendar::update(&graph, &self.store, id, &draft).await.map(|_| ()),
                    None => calendar::create(&graph, &self.store, &draft).await.map(|_| ()),
                };
                match result {
                    Ok(()) => {
                        self.emit(Event::CalendarSaved);
                        self.emit(Event::Calendar);
                        // Recurring events and their instances come from the server.
                        let (start, end) = *self.calendar_window.lock().unwrap();
                        sync::sync_events(&graph, &self.store, &draft.calendar_id, start, end).await.ok();
                        self.emit(Event::Calendar);
                    }
                    Err(error) => self.emit(Event::CalendarFailed(message_of(&error))),
                }
            }
            Request::DeleteEvent { account, id, cancel } => {
                let Some(graph) = self.graph(&account) else { return };
                let result = calendar::delete(&graph, &self.store, &id, cancel.then_some("")).await;
                self.emit(Event::Calendar);
                if let Err(error) = result {
                    self.report("Deleting the event", &error);
                }
            }
            Request::Respond { account, id, answer } => {
                let Some(graph) = self.graph(&account) else { return };
                let result = calendar::respond(&graph, &self.store, &id, answer, "", true).await;
                self.emit(Event::Calendar);
                match result {
                    Ok(()) => self.emit(Event::Status("Response sent".into())),
                    Err(error) => self.report("Responding", &error),
                }
            }
            Request::SaveContact { contact } => {
                let Some(graph) = self.graph(&contact.account) else { return };
                let result = if contact.id.is_empty() {
                    people::create(&graph, &self.store, &contact).await
                } else {
                    people::update(&graph, &self.store, &contact).await
                };
                match result {
                    Ok(_) => {
                        self.emit(Event::ContactSaved);
                        self.emit(Event::Contacts);
                    }
                    Err(error) => self.emit(Event::ContactFailed(message_of(&error))),
                }
            }
            Request::DeleteContact { account, id } => {
                let Some(graph) = self.graph(&account) else { return };
                let result = people::delete(&graph, &self.store, &id).await;
                self.emit(Event::Contacts);
                if let Err(error) = result {
                    self.report("Deleting the contact", &error);
                }
            }
            Request::Configure(config) => {
                let changed_client = config.client_id != self.config.lock().unwrap().client_id;
                *self.config.lock().unwrap() = config;
                if changed_client {
                    self.accounts.lock().unwrap().clear();
                    self.restore_accounts();
                }
            }
        }
    }

    async fn sign_in(&self) {
        let client_id = match self.config.lock().unwrap().client_id() {
            Ok(id) => id,
            Err(error) => {
                self.emit(Event::SignInFailed(error.to_string()));
                return;
            }
        };
        self.emit(Event::SigningIn(true));
        match auth::sign_in(self.http.clone(), &client_id).await {
            Ok((session, account)) => {
                if let Err(error) = self.store.lock().unwrap().put_account(&account) {
                    self.emit(Event::SignInFailed(error.to_string()));
                    return;
                }
                let graph = Graph::new(self.http.clone(), Arc::new(session));
                self.accounts.lock().unwrap().insert(account.id.clone(), graph.clone());
                self.emit(Event::SigningIn(false));
                self.emit(Event::Accounts(self.store_accounts()));
                self.emit(Event::Status(format!("Signed in as {}", account.email)));
                self.emit(Event::Syncing(true));
                self.sync_mail(&graph).await;
                let (start, end) = *self.calendar_window.lock().unwrap();
                self.sync_calendar(&graph, start, end).await;
                self.emit(Event::Calendar);
                if let Err(error) = sync::sync_contacts(&graph, &self.store).await {
                    self.report("Updating contacts", &error);
                }
                self.emit(Event::Contacts);
                self.emit(Event::Syncing(false));
            }
            Err(error) => {
                log::warn!("sign-in: {error}");
                self.emit(Event::SigningIn(false));
                self.emit(Event::SignInFailed(error.to_string()));
            }
        }
    }

    fn folder_of(&self, account: &str, id: &str) -> Option<String> {
        self.store.lock().unwrap().message(account, id).ok().flatten().map(|detail| detail.summary.folder_id)
    }

    fn changed_message(&self, account: &str, id: &str) {
        if let Some(folder) = self.folder_of(account, id) {
            self.emit(Event::Messages { account: account.to_owned(), folder });
        }
        self.emit(Event::Folders);
    }

    fn after_move(&self, account: &str, source: Option<String>) {
        if let Some(folder) = source {
            self.emit(Event::Messages { account: account.to_owned(), folder });
        }
        self.emit(Event::Folders);
    }

    async fn prepare_original(&self, graph: &Graph, id: &str, remote: bool) -> Result<PathBuf, String> {
        let detail = mail::open(graph, &self.store, id).await.map_err(|e| e.to_string())?.ok_or("the message is gone")?;
        let html = detail.body_html.unwrap_or_default();
        let lower = html.to_ascii_lowercase();
        let mut pictures = Vec::new();
        for (index, attachment) in detail.attachments.iter().enumerate() {
            let Some(content_id) = attachment.content_id.as_deref().map(|id| id.trim_matches(['<', '>'])) else { continue };
            let reference = format!("cid:{content_id}");
            if !lower.contains(&reference.to_ascii_lowercase()) {
                continue;
            }
            let extension = attachment.name.rsplit_once('.').map(|(_, ext)| ext).unwrap_or("bin");
            let extension: String = extension.chars().filter(char::is_ascii_alphanumeric).take(5).collect();
            if let Ok(bytes) = mail::attachment_bytes(graph, id, &attachment.id).await {
                pictures.push((reference, format!("inline-{index}.{extension}"), bytes));
            }
        }
        let dir = std::env::temp_dir().join("lookout").join(format!("original-{}", detail.summary.received));
        crate::viewer::write_page(&dir, &html, &pictures, remote).map_err(|e| e.to_string())
    }

    async fn pictures(
        &self,
        graph: &Graph,
        message: &str,
        sources: &[String],
        remote: bool,
    ) -> Vec<(String, SharedPixelBuffer<Rgba8Pixel>)> {
        let attachments = self.attachments(graph, message);
        let mut pictures = Vec::new();
        for source in sources {
            let Some(bytes) = self.resource(graph, message, &attachments, source, remote).await else { continue };
            let decoded = tokio::task::spawn_blocking(move || decode_picture(&bytes)).await.ok().flatten();
            if let Some(buffer) = decoded {
                pictures.push((source.clone(), buffer));
            }
        }
        pictures
    }

    fn attachments(&self, graph: &Graph, message: &str) -> Vec<Attachment> {
        self.store
            .lock()
            .unwrap()
            .message(graph.account_id(), message)
            .ok()
            .flatten()
            .map(|detail| detail.attachments)
            .unwrap_or_default()
    }

    /// The bytes `source` points at in a message: an inline attachment
    /// (`cid:`), data in the address, or, if `remote`, a download.
    async fn resource(
        &self,
        graph: &Graph,
        message: &str,
        attachments: &[Attachment],
        source: &str,
        remote: bool,
    ) -> Option<Vec<u8>> {
        let lower = source.to_ascii_lowercase();
        if let Some(content_id) = lower.strip_prefix("cid:") {
            let content_id = content_id.trim_matches(['<', '>']);
            let attachment = attachments.iter().find(|attachment| {
                attachment.content_id.as_deref().is_some_and(|id| id.trim_matches(['<', '>']).eq_ignore_ascii_case(content_id))
                    || attachment.name.eq_ignore_ascii_case(content_id)
            })?;
            mail::attachment_bytes(graph, message, &attachment.id).await.ok()
        } else if lower.starts_with("data:") {
            source.split_once(";base64,").and_then(|(_, data)| {
                use base64::Engine as _;
                base64::engine::general_purpose::STANDARD.decode(data.trim()).ok()
            })
        } else if remote && (lower.starts_with("https://") || lower.starts_with("http://")) {
            match self.http.get(source).send().await {
                Ok(response) if response.status().is_success() => response.bytes().await.ok().map(|b| b.to_vec()),
                _ => None,
            }
        } else {
            None
        }
    }
}

/// Decodes a picture, shrunk to fit the reading pane.
fn decode_picture(bytes: &[u8]) -> Option<SharedPixelBuffer<Rgba8Pixel>> {
    let image = image::load_from_memory(bytes).ok()?;
    let image = if image.width() > MAX_PICTURE_WIDTH || image.height() > MAX_PICTURE_HEIGHT {
        image.resize(MAX_PICTURE_WIDTH, MAX_PICTURE_HEIGHT, image::imageops::FilterType::Triangle)
    } else {
        image
    };
    let rgba = image.into_rgba8();
    Some(SharedPixelBuffer::clone_from_slice(rgba.as_raw(), rgba.width(), rgba.height()))
}

/// A picture larger than a message drawn by Blitz could show, made smaller;
/// anything else (style sheets, fonts, drawings) as it is.
fn shrink_picture(bytes: Vec<u8>) -> Vec<u8> {
    use std::io::Cursor;
    let Ok(reader) = image::ImageReader::new(Cursor::new(&bytes)).with_guessed_format() else { return bytes };
    if reader.format().is_none() {
        return bytes;
    }
    match reader.into_dimensions() {
        Ok((width, height)) if width > MAX_PAGE_PICTURE_WIDTH || height > MAX_PAGE_PICTURE_HEIGHT => {}
        _ => return bytes,
    }
    let Ok(image) = image::load_from_memory(&bytes) else { return bytes };
    let image = image.resize(MAX_PAGE_PICTURE_WIDTH, MAX_PAGE_PICTURE_HEIGHT, image::imageops::FilterType::Triangle);
    let mut out = Cursor::new(Vec::new());
    // Photos stay small as JPEG; PNG keeps transparency.
    let written = if image.color().has_alpha() {
        image.write_to(&mut out, image::ImageFormat::Png)
    } else {
        image.into_rgb8().write_to(&mut out, image::ImageFormat::Jpeg)
    };
    match written {
        Ok(()) => out.into_inner(),
        Err(_) => bytes,
    }
}

/// Writes an attachment to Downloads, or to a temporary folder when it is
/// only being opened. Never overwrites: a number is added instead.
fn save_download(name: &str, bytes: &[u8], temporary: bool) -> std::io::Result<PathBuf> {
    let dir = if temporary {
        std::env::temp_dir().join("lookout")
    } else {
        dirs::download_dir().unwrap_or_else(std::env::temp_dir)
    };
    std::fs::create_dir_all(&dir)?;
    let safe: String = name
        .chars()
        .map(|c| if matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') || c.is_control() { '_' } else { c })
        .collect();
    let safe = if safe.trim().is_empty() { "attachment".to_owned() } else { safe };
    let (stem, extension) = match safe.rsplit_once('.') {
        Some((stem, extension)) if !stem.is_empty() => (stem.to_owned(), format!(".{extension}")),
        _ => (safe.clone(), String::new()),
    };
    let mut path = dir.join(&safe);
    let mut number = 1;
    while path.exists() {
        path = dir.join(format!("{stem} ({number}){extension}"));
        number += 1;
    }
    std::fs::write(&path, bytes)?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png(width: u32, height: u32) -> Vec<u8> {
        let mut out = std::io::Cursor::new(Vec::new());
        image::RgbImage::new(width, height).write_to(&mut out, image::ImageFormat::Png).unwrap();
        out.into_inner()
    }

    #[test]
    fn only_large_pictures_shrink() {
        let small = png(400, 300);
        assert_eq!(shrink_picture(small.clone()), small);
        let css = b"body { color: red }".to_vec();
        assert_eq!(shrink_picture(css.clone()), css);
        let shrunk = image::load_from_memory(&shrink_picture(png(4000, 3000))).unwrap();
        assert_eq!((shrunk.width(), shrunk.height()), (MAX_PAGE_PICTURE_WIDTH, 1350));
    }
}
