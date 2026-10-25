//! The mail page's behaviour.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

use lookout_core::html::{self, Rendered};
use lookout_core::mail::{Bulk, Draft, NewAttachment, ReplyKind};
use lookout_core::model::{Address, Flag, MessageDetail, MessageSummary, wire};
use lookout_core::{Store, when};
use slint::{ComponentHandle, Model, ModelRc, Rgba8Pixel, SharedPixelBuffer, SharedString, Timer, TimerMode, VecModel};

use crate::app::App;
use crate::engine::Request;
use crate::models::MessageList;
use crate::{AttachmentRow, BodyBlock, Compose, FolderRow, Icons, Mail, MainWindow, Reader, body, format};

// Typing in the search box searches after this pause.
const SEARCH_DELAY: Duration = Duration::from_millis(250);
const SEARCH_LIMIT: usize = 500;
const SUGGESTIONS: usize = 6;
// Graph refuses attachments larger than this.
const MAX_ATTACHMENT: u64 = 150 * 1024 * 1024;
// Of the original, shown under a reply while writing it.
const QUOTE_PREVIEW_CHARS: usize = 3000;

/// A picture's block, its place in the block's row, and the size the
/// sender gave it.
type PictureSpot = (usize, usize, (Option<u32>, Option<u32>));

#[derive(Debug, Clone, PartialEq, Eq)]
enum View {
    None,
    Folder { account: String, folder: String },
    Search { query: String },
}

pub struct MailState {
    list: Rc<MessageList>,
    view: RefCell<View>,
    unread_only: Cell<bool>,
    selected: RefCell<Option<(String, String)>>,
    open_token: Cell<u64>,
    detail: RefCell<Option<MessageDetail>>,
    blocks: Rc<VecModel<BodyBlock>>,
    /// Where each picture source is shown: block, place in its row, and
    /// the size the sender gave.
    picture_blocks: RefCell<HashMap<String, Vec<PictureSpot>>>,
    remote_shown: Cell<bool>,
    attachments: RefCell<Vec<NewAttachment>>,
    reply: RefCell<Option<(ReplyKind, String)>>,
    reply_draft: RefCell<Option<wire::Message>>,
    reply_token: Cell<u64>,
    existing_draft: RefCell<Option<String>>,
    /// What the Move dialog moves: one message or a selection.
    move_target: RefCell<Vec<(String, String)>>,
    /// The messages being dragged towards a folder.
    dragged: RefCell<Vec<(String, String)>>,
    /// After a message leaves the list, the next one at this place opens.
    select_after: Cell<Option<usize>>,
    search_timer: Timer,
    search_token: Cell<u64>,
    original: OriginalState,
    /// The open message as sent, drawn by Blitz.
    page: RefCell<Option<crate::page::Page>>,
    /// The pane's view of the page: width, height, and scroll.
    page_view: Cell<(f32, f32, f32, f32)>,
}

/// The reading pane's WebView2 mode.
#[derive(Default)]
struct OriginalState {
    /// Where the panel goes, in logical pixels of the window.
    area: Cell<(f32, f32, f32, f32)>,
    /// Nothing covers the pane.
    shown: Cell<bool>,
    token: Cell<u64>,
    /// The page for the open message, once written.
    page: RefCell<Option<PathBuf>>,
    /// The page the panel has loaded.
    loaded: RefCell<Option<PathBuf>>,
    #[cfg(windows)]
    panel: RefCell<Option<crate::viewer::Embedded>>,
}

impl MailState {
    pub fn new(store: Rc<Store>) -> Self {
        Self {
            list: MessageList::new(store),
            view: RefCell::new(View::None),
            unread_only: Cell::new(false),
            selected: RefCell::new(None),
            open_token: Cell::new(0),
            detail: RefCell::new(None),
            blocks: Rc::new(VecModel::default()),
            picture_blocks: RefCell::new(HashMap::new()),
            remote_shown: Cell::new(false),
            attachments: RefCell::new(Vec::new()),
            reply: RefCell::new(None),
            reply_draft: RefCell::new(None),
            reply_token: Cell::new(0),
            existing_draft: RefCell::new(None),
            move_target: RefCell::new(Vec::new()),
            dragged: RefCell::new(Vec::new()),
            select_after: Cell::new(None),
            search_timer: Timer::default(),
            search_token: Cell::new(0),
            original: OriginalState::default(),
            page: RefCell::new(None),
            page_view: Cell::new((0.0, 0.0, 0.0, 0.0)),
        }
    }
}

macro_rules! with_app {
    ($app:expr, |$name:ident $(, $arg:ident)*| $body:expr) => {{
        let weak = Rc::downgrade($app);
        move |$($arg),*| {
            if let Some($name) = weak.upgrade() {
                $body
            }
        }
    }};
}

pub fn wire(app: &Rc<App>, ui: &MainWindow) {
    let mail = ui.global::<Mail>();
    mail.set_messages(ModelRc::from(app.mail.list.clone()));
    mail.set_blocks(ModelRc::from(app.mail.blocks.clone()));
    // Slint 1.18 only builds drag payloads through this API; drags without
    // one never start.
    let mut payload = slint::private_unstable_api::re_exports::DataTransfer::default();
    payload.set_plain_text("Lookout messages".into());
    mail.set_drag_payload(payload);

    mail.on_folder_clicked(with_app!(app, |app, account, folder| open_folder(&app, &account, &folder)));
    mail.on_toggle_expanded(with_app!(app, |app, account, folder| toggle_expanded(&app, &account, &folder)));
    mail.on_toggle_favourite(with_app!(app, |app, account, folder| toggle_favourite(&app, &account, &folder)));
    mail.on_select(with_app!(app, |app, index, control, shift| click(&app, index, control, shift)));
    mail.on_drag_started(with_app!(app, |app, index| {
        if !app.mail.list.is_selected(index.max(0) as usize) {
            select(&app, index);
        }
        *app.mail.dragged.borrow_mut() =
            app.mail.list.selection().into_iter().map(|summary| (summary.account, summary.id)).collect();
    }));
    mail.on_dropped_on(with_app!(app, |app, account, folder| {
        let dragged = std::mem::take(&mut *app.mail.dragged.borrow_mut());
        move_messages(&app, dragged, &account, &folder);
    }));
    mail.on_row_clicked(with_app!(app, |app, index| {
        if app.mail.list.selection_len() > 1 && app.mail.list.is_selected(index.max(0) as usize) {
            select(&app, index);
        }
    }));
    mail.on_bulk(with_app!(app, |app, action| bulk(&app, &action)));
    mail.on_clear_selection(with_app!(app, |app| {
        let selected = app.ui().map(|ui| ui.global::<Mail>().get_selected()).unwrap_or(-1);
        if selected >= 0 {
            app.mail.list.select_one(selected as usize);
        }
        update_selection_count(&app);
    }));
    mail.on_search(with_app!(app, |app, text| search_typed(&app, text)));
    mail.on_search_server(with_app!(app, |app| search_server(&app)));
    mail.on_toggle_unread_only(with_app!(app, |app| {
        app.mail.unread_only.set(!app.mail.unread_only.get());
        if let Some(ui) = app.ui() {
            ui.global::<Mail>().set_unread_only(app.mail.unread_only.get());
        }
        reload(&app);
    }));
    mail.on_toggle_flag_at(with_app!(app, |app, index| toggle_flag(&app, index)));
    mail.on_toggle_read_at(with_app!(app, |app, index| toggle_read(&app, index)));
    mail.on_delete_at(with_app!(app, |app, index| remove(&app, index, None)));
    mail.on_archive_at(with_app!(app, |app, index| remove(&app, index, Some("archive".into()))));
    mail.on_move_at(with_app!(app, |app, index| open_move(&app, index)));
    mail.on_move_to(with_app!(app, |app, folder| move_to(&app, &folder)));
    mail.on_load_more(|| {});
    mail.on_new_message(with_app!(app, |app| new_message(&app, Vec::new(), String::new())));
    mail.on_reply(with_app!(app, |app, kind| reply(&app, kind)));
    mail.on_edit_draft(with_app!(app, |app| edit_draft(&app)));
    mail.on_show_images(with_app!(app, |app| {
        app.mail.remote_shown.set(true);
        if app.mail.page.borrow().is_some() {
            open_page(&app, true);
        } else {
            request_pictures(&app, true);
        }
        if app.config.borrow().original_view && webview(&app) {
            request_original(&app);
        }
        update_reader(&app, |reader| reader.images_shown = true);
    }));
    mail.set_original(app.config.borrow().original_view);
    mail.set_webview(webview(app));
    mail.on_page_view(with_app!(app, |app, width, height, x, y| {
        app.mail.page_view.set((width, height, x, y));
        paint_page(&app);
    }));
    mail.on_page_clicked(with_app!(app, |app, x, y| {
        if let Some(link) = page_link(&app, x, y) {
            open_link(&app, &link);
        }
    }));
    mail.on_page_over_link({
        let weak = Rc::downgrade(app);
        move |x, y| weak.upgrade().is_some_and(|app| page_link(&app, x, y).is_some())
    });
    mail.on_toggle_original(with_app!(app, |app| toggle_original(&app)));
    mail.on_original_area(with_app!(app, |app, x, y, width, height| {
        app.mail.original.area.set((x, y, width, height));
        update_original(&app);
    }));
    mail.on_original_shown(with_app!(app, |app, shown| {
        app.mail.original.shown.set(shown);
        update_original(&app);
    }));
    mail.on_open_attachment(with_app!(app, |app, id| attachment(&app, &id, true)));
    mail.on_save_attachment(with_app!(app, |app, id| attachment(&app, &id, false)));
    mail.on_link_clicked(with_app!(app, |app, link| open_link(&app, &link)));
    mail.on_send(with_app!(app, |app, compose| send(&app, compose, false)));
    mail.on_save_draft(with_app!(app, |app, compose| send(&app, compose, true)));
    mail.on_discard(with_app!(app, |app| close_compose(&app)));
    mail.on_attach(with_app!(app, |app| attach(&app)));
    mail.on_remove_attachment(with_app!(app, |app, id| {
        if let Ok(index) = id.parse::<usize>() {
            let mut attachments = app.mail.attachments.borrow_mut();
            if index < attachments.len() {
                attachments.remove(index);
            }
        }
        show_compose_attachments(&app);
    }));
    mail.on_address_typed(with_app!(app, |app, field, text| suggest(&app, field, &text)));
    mail.on_format(|kind, text, anchor, cursor| {
        let edit = crate::editor::format(&kind, &text, anchor.max(0) as usize, cursor.max(0) as usize);
        crate::FormatResult { text: edit.text.into(), anchor: edit.anchor as i32, cursor: edit.cursor as i32 }
    });
    mail.on_preview(with_app!(app, |app, text| {
        let rendered = html::render(&html::markdown_to_html(&text));
        let blocks = body::blocks(&rendered, app.config.borrow().dark);
        if let Some(ui) = app.ui() {
            ui.global::<Mail>().set_preview_blocks(ModelRc::new(VecModel::from(blocks)));
        }
    }));
    let weak = Rc::downgrade(app);
    mail.on_suggestion_picked(move |_field, current, picked| {
        if let Some(app) = weak.upgrade()
            && let Some(ui) = app.ui()
        {
            let mail = ui.global::<Mail>();
            mail.set_suggestions(ModelRc::default());
            mail.set_suggestion_field(-1);
        }
        format::complete_address(&current, &picked).into()
    });
}

pub fn accounts_changed(app: &Rc<App>) {
    folders_changed(app);
    ensure_view(app);
}

/// Opens the first inbox when nothing valid is shown.
fn ensure_view(app: &Rc<App>) {
    let current = app.mail.view.borrow().clone();
    let account_exists = |id: &str| app.accounts.borrow().iter().any(|a| a.id == id);
    let stale = match &current {
        View::Folder { account, .. } => !account_exists(account),
        View::None => true,
        View::Search { .. } => false,
    };
    if stale {
        let first = app.accounts.borrow().first().map(|account| account.id.clone());
        match first.and_then(|account| Some((app.store.well_known_folder(&account, "inbox").ok()??, account))) {
            Some((inbox, account)) => open_folder(app, &account, &inbox),
            None => {
                *app.mail.view.borrow_mut() = View::None;
                app.mail.list.set_rows(Vec::new());
                close_reader(app);
            }
        }
    }
}

fn icon_for(ui: &MainWindow, well_known: Option<&str>) -> SharedString {
    let icons = ui.global::<Icons>();
    match well_known {
        Some("inbox") => icons.get_inbox(),
        Some("drafts") => icons.get_edit(),
        Some("sentitems") | Some("outbox") => icons.get_send(),
        Some("deleteditems") => icons.get_delete(),
        Some("junkemail") => icons.get_junk(),
        Some("archive") => icons.get_archive(),
        _ => icons.get_folder(),
    }
}

fn folder_key(account: &str, folder: &str) -> String {
    format!("{account}/{folder}")
}

/// The folders as the pane lists them: favourites, then each account's
/// tree without what is collapsed. With `account_filter`, one account's
/// whole tree, for the Move dialog.
fn folder_rows(app: &App, ui: &MainWindow, account_filter: Option<&str>) -> Vec<FolderRow> {
    let pane = account_filter.is_none();
    let config = app.config.borrow();
    let collapsed: std::collections::HashSet<&str> = config.collapsed.iter().map(String::as_str).collect();
    let several = app.accounts.borrow().len() > 1;
    let mut trees = Vec::new();
    let mut by_key: HashMap<String, FolderRow> = HashMap::new();
    for account in app.accounts.borrow().iter() {
        if account_filter.is_some_and(|only| only != account.id) {
            continue;
        }
        let account_key = folder_key(&account.id, "");
        let account_open = !pane || !collapsed.contains(account_key.as_str());
        if pane {
            trees.push(FolderRow {
                account: account.id.as_str().into(),
                name: account.email.as_str().into(),
                header: true,
                expandable: true,
                expanded: account_open,
                ..FolderRow::default()
            });
        }
        let folders = app.store.folders(&account.id).unwrap_or_default();
        let parents: std::collections::HashSet<&str> =
            folders.iter().filter_map(|folder| folder.parent_id.as_deref()).collect();
        let mut depth: HashMap<&str, i32> = HashMap::new();
        // Folders under a collapsed one are skipped; this tracks them.
        let mut hidden: std::collections::HashSet<&str> = std::collections::HashSet::new();
        // Standard folders come first; a line sets the user's own apart.
        let mut divided = false;
        for folder in &folders {
            let level = folder.parent_id.as_deref().and_then(|parent| depth.get(parent)).map(|d| d + 1).unwrap_or(0);
            depth.insert(&folder.id, level);
            let key = folder_key(&account.id, &folder.id);
            let parent_hidden = folder.parent_id.as_deref().is_some_and(|parent| {
                hidden.contains(parent) || (pane && collapsed.contains(folder_key(&account.id, parent).as_str()))
            });
            if parent_hidden {
                hidden.insert(&folder.id);
            }
            // An empty Outbox is clutter.
            if folder.well_known.as_deref() == Some("outbox") && folder.total == 0 {
                continue;
            }
            let unread = match folder.well_known.as_deref() {
                // Counts of these folders aren't news.
                Some("drafts") => folder.total,
                Some("sentitems") | Some("deleteditems") | Some("junkemail") => 0,
                _ => folder.unread,
            };
            let expandable = parents.contains(folder.id.as_str());
            let row = FolderRow {
                account: account.id.as_str().into(),
                id: folder.id.as_str().into(),
                name: folder.name.as_str().into(),
                depth: level,
                unread: unread as i32,
                icon: icon_for(ui, folder.well_known.as_deref()),
                header: false,
                expandable,
                expanded: expandable && !(pane && collapsed.contains(key.as_str())),
                favourite: config.favourites.contains(&key),
                divider: false,
            };
            if pane {
                let name = if several { format!("{} · {}", folder.name, account.email) } else { folder.name.clone() };
                by_key.insert(key, FolderRow { name: name.into(), depth: 0, expandable: false, ..row.clone() });
            }
            if account_open && !parent_hidden {
                let own = folder.parent_id.is_none() && folder.well_known.is_none();
                if pane && own && !divided {
                    divided = true;
                    trees.push(FolderRow { account: account.id.as_str().into(), divider: true, ..FolderRow::default() });
                }
                trees.push(row);
            }
        }
    }
    let mut rows = Vec::new();
    if pane {
        let favourites: Vec<FolderRow> = config.favourites.iter().filter_map(|key| by_key.remove(key)).collect();
        if !favourites.is_empty() {
            rows.push(FolderRow { name: "Favourites".into(), header: true, ..FolderRow::default() });
            rows.extend(favourites);
        }
    }
    rows.extend(trees);
    rows
}

fn toggle_expanded(app: &Rc<App>, account: &str, folder: &str) {
    let key = folder_key(account, folder);
    {
        let mut config = app.config.borrow_mut();
        if let Some(at) = config.collapsed.iter().position(|k| *k == key) {
            config.collapsed.remove(at);
        } else {
            config.collapsed.push(key);
        }
    }
    save_config(app);
    folders_changed(app);
}

fn toggle_favourite(app: &Rc<App>, account: &str, folder: &str) {
    let key = folder_key(account, folder);
    {
        let mut config = app.config.borrow_mut();
        if let Some(at) = config.favourites.iter().position(|k| *k == key) {
            config.favourites.remove(at);
        } else {
            config.favourites.push(key);
        }
    }
    save_config(app);
    folders_changed(app);
}

fn save_config(app: &App) {
    if let Err(error) = app.config.borrow().save() {
        log::warn!("saving settings: {error}");
    }
}

pub fn folders_changed(app: &Rc<App>) {
    let Some(ui) = app.ui() else { return };
    let rows = folder_rows(app, &ui, None);
    let inbox_unread: i32 = app
        .accounts
        .borrow()
        .iter()
        .filter_map(|account| {
            let inbox = app.store.well_known_folder(&account.id, "inbox").ok()??;
            Some(app.store.folders(&account.id).ok()?.into_iter().find(|f| f.id == inbox)?.unread as i32)
        })
        .sum();
    ui.set_window_title(if inbox_unread > 0 { format!("Lookout ({inbox_unread})") } else { "Lookout".into() }.into());
    ui.global::<Mail>().set_folders(ModelRc::new(VecModel::from(rows)));
    // The first sync of a new account: open its inbox once it is known.
    if *app.mail.view.borrow() == View::None {
        ensure_view(app);
    }
    update_title(app);
}

fn update_title(app: &App) {
    let Some(ui) = app.ui() else { return };
    let title = match &*app.mail.view.borrow() {
        View::Folder { account, folder } => app
            .store
            .folders(account)
            .ok()
            .and_then(|folders| folders.into_iter().find(|f| &f.id == folder))
            .map(|folder| folder.name)
            .unwrap_or_default(),
        View::Search { .. } => "Search results".into(),
        View::None => String::new(),
    };
    ui.global::<Mail>().set_list_title(title.into());
}

pub fn open_folder(app: &Rc<App>, account: &str, folder: &str) {
    let Some(ui) = app.ui() else { return };
    *app.mail.view.borrow_mut() = View::Folder { account: account.into(), folder: folder.into() };
    let mail = ui.global::<Mail>();
    mail.set_selected_account(account.into());
    mail.set_selected_folder(folder.into());
    mail.set_searching(false);
    mail.set_search_text("".into());
    *app.mail.selected.borrow_mut() = None;
    mail.set_selected(-1);
    close_reader(app);
    reload(app);
    update_title(app);
    app.engine.send(Request::Watch { account: account.into(), folder: folder.into() });
}

/// Opens a message in its folder, as a click on its notification does.
pub fn show_message(app: &Rc<App>, account: &str, folder: &str, id: &str) {
    let Some(ui) = app.ui() else { return };
    ui.global::<crate::Shell>().set_page(crate::Page::Mail);
    let shown = matches!(&*app.mail.view.borrow(), View::Folder { account: a, folder: f } if a == account && f == folder);
    if !shown {
        open_folder(app, account, folder);
    }
    if let Some(index) = app.mail.list.index_of(account, id) {
        select(app, index as i32);
    }
}

/// Reads the list again, keeping the selection.
fn reload(app: &Rc<App>) {
    let rows = match &*app.mail.view.borrow() {
        View::Folder { account, folder } => {
            app.store.folder_rows(account, folder, app.mail.unread_only.get()).unwrap_or_default()
        }
        View::Search { query } => app.store.search_rows(None, query, SEARCH_LIMIT).unwrap_or_default(),
        View::None => Vec::new(),
    };
    app.mail.list.set_rows(rows);
    update_selection_count(app);
    let Some(ui) = app.ui() else { return };
    let mail = ui.global::<Mail>();
    let selected = app.mail.selected.borrow().clone();
    let index = selected.and_then(|(account, id)| app.mail.list.index_of(&account, &id));
    match (index, app.mail.select_after.take()) {
        (Some(index), _) => mail.set_selected(index as i32),
        (None, Some(after)) if app.mail.list.len() > 0 => select(app, after.min(app.mail.list.len() - 1) as i32),
        _ => mail.set_selected(-1),
    }
}

pub fn messages_changed(app: &Rc<App>, account: &str, folder: &str) {
    let shown = match &*app.mail.view.borrow() {
        View::Folder { account: a, folder: f } => a == account && f == folder,
        View::Search { .. } => true,
        View::None => false,
    };
    if shown {
        reload(app);
    }
    // The open message may have changed (read elsewhere, flagged…).
    let selected = app.mail.selected.borrow().clone();
    if let Some((account, id)) = selected
        && let Ok(Some(detail)) = app.store.message(&account, &id)
    {
        update_reader(app, |reader| {
            reader.unread = !detail.summary.is_read;
            reader.flagged = detail.summary.flag == Flag::Flagged;
        });
    }
}

fn search_typed(app: &Rc<App>, text: SharedString) {
    let weak = Rc::downgrade(app);
    app.mail.search_timer.start(TimerMode::SingleShot, SEARCH_DELAY, move || {
        let Some(app) = weak.upgrade() else { return };
        let query = text.trim().to_owned();
        let Some(ui) = app.ui() else { return };
        if query.is_empty() {
            let back = app.store_view_before_search();
            ui.global::<Mail>().set_searching(false);
            if let Some((account, folder)) = back {
                open_folder(&app, &account, &folder);
            }
            return;
        }
        if !matches!(*app.mail.view.borrow(), View::Search { .. }) {
            app.remember_view_before_search();
        }
        *app.mail.view.borrow_mut() = View::Search { query };
        ui.global::<Mail>().set_searching(true);
        reload(&app);
        update_title(&app);
    });
}

thread_local! {
    static BEFORE_SEARCH: RefCell<Option<(String, String)>> = const { RefCell::new(None) };
}

impl App {
    fn remember_view_before_search(&self) {
        if let View::Folder { account, folder } = &*self.mail.view.borrow() {
            BEFORE_SEARCH.with(|slot| *slot.borrow_mut() = Some((account.clone(), folder.clone())));
        }
    }

    fn store_view_before_search(&self) -> Option<(String, String)> {
        BEFORE_SEARCH.with(|slot| slot.borrow_mut().take())
    }
}

fn search_server(app: &Rc<App>) {
    let View::Search { query } = app.mail.view.borrow().clone() else { return };
    let token = app.token();
    app.mail.search_token.set(token);
    if let Some(ui) = app.ui() {
        ui.global::<Mail>().set_busy(true);
    }
    app.engine.send(Request::SearchServer { query, token });
}

pub fn search_results(app: &Rc<App>, token: u64, ids: Vec<(String, String)>) {
    if token != app.mail.search_token.get() {
        return;
    }
    let Some(ui) = app.ui() else { return };
    ui.global::<Mail>().set_busy(false);
    if !matches!(*app.mail.view.borrow(), View::Search { .. }) {
        return;
    }
    // The server's matches first, then the cache's.
    let mut rows: Vec<i64> = ids.iter().filter_map(|(account, id)| app.store.row_of(account, id).ok().flatten()).collect();
    if let View::Search { query } = &*app.mail.view.borrow() {
        for row in app.store.search_rows(None, query, SEARCH_LIMIT).unwrap_or_default() {
            if !rows.contains(&row) {
                rows.push(row);
            }
        }
    }
    app.mail.list.set_rows(rows);
    ui.global::<Mail>().set_selected(-1);
    app.status(&format!("{} found on the server", ids.len()));
}

/// A click on a row: opens it, or with Ctrl or Shift adds to the selection.
fn click(app: &Rc<App>, index: i32, control: bool, shift: bool) {
    if index < 0 {
        return;
    }
    if shift {
        app.mail.list.select_range(index as usize);
    } else if control {
        app.mail.list.toggle(index as usize);
    } else if app.mail.list.selection_len() > 1 && app.mail.list.is_selected(index as usize) {
        // Maybe the start of dragging the selection; a plain click picks
        // the row alone on release (see row-clicked).
        return;
    } else {
        select(app, index);
        return;
    }
    update_selection_count(app);
    // Down to one again: show it.
    if app.mail.list.selection_len() == 1
        && let Some(only) = app.mail.list.selection().first()
        && let Some(position) = app.mail.list.index_of(&only.account, &only.id)
    {
        select(app, position as i32);
    }
}

fn update_selection_count(app: &App) {
    if let Some(ui) = app.ui() {
        ui.global::<Mail>().set_selection_count(app.mail.list.selection_len() as i32);
    }
}

/// The messages an action at `index` applies to: the selection if the row
/// is part of it, else the row alone.
fn targets_at(app: &App, index: i32) -> Vec<MessageSummary> {
    if index >= 0 && app.mail.list.selection_len() > 1 && app.mail.list.is_selected(index as usize) {
        app.mail.list.selection()
    } else {
        summary_at(app, index).into_iter().collect()
    }
}

/// Opens the message at `index`, selecting it alone.
fn select(app: &Rc<App>, index: i32) {
    let Some(ui) = app.ui() else { return };
    let Some(summary) = (index >= 0).then(|| app.mail.list.summary(index as usize)).flatten() else { return };
    app.mail.list.select_one(index as usize);
    update_selection_count(app);
    let mail = ui.global::<Mail>();
    mail.set_selected(index);
    if app.mail.selected.borrow().as_ref() == Some(&(summary.account.clone(), summary.id.clone())) && mail.get_reader().open {
        return;
    }
    *app.mail.selected.borrow_mut() = Some((summary.account.clone(), summary.id.clone()));
    show_header(app, &summary);
    let token = app.token();
    app.mail.open_token.set(token);
    app.engine.send(Request::Open { account: summary.account.clone(), id: summary.id.clone(), token });
}

fn show_header(app: &App, summary: &MessageSummary) {
    let Some(ui) = app.ui() else { return };
    let mail = ui.global::<Mail>();
    app.mail.blocks.set_vec(Vec::new());
    app.mail.picture_blocks.borrow_mut().clear();
    app.mail.remote_shown.set(app.config.borrow().load_remote_images);
    *app.mail.detail.borrow_mut() = None;
    *app.mail.original.page.borrow_mut() = None;
    close_page(app);
    mail.set_attachments(ModelRc::default());
    mail.set_select_text(false);
    mail.set_reader(Reader {
        open: true,
        loading: true,
        account: summary.account.as_str().into(),
        id: summary.id.as_str().into(),
        subject: summary.subject.as_str().into(),
        sender: summary.from.display().into(),
        sender_address: summary.from.address.as_str().into(),
        initials: format::initials(summary.from.display()).into(),
        to: format::names(&summary.to).into(),
        cc: SharedString::new(),
        date: when::long_date(summary.received).into(),
        flagged: summary.flag == Flag::Flagged,
        unread: !summary.is_read,
        draft: summary.is_draft,
        remote_images: false,
        images_shown: app.mail.remote_shown.get(),
        truncated: false,
        plain: SharedString::new(),
        categories: SharedString::new(),
    });
}

fn close_reader(app: &App) {
    let Some(ui) = app.ui() else { return };
    app.mail.blocks.set_vec(Vec::new());
    app.mail.picture_blocks.borrow_mut().clear();
    *app.mail.detail.borrow_mut() = None;
    close_page(app);
    ui.global::<Mail>().set_reader(Reader::default());
    ui.global::<Mail>().set_attachments(ModelRc::default());
}

fn update_reader(app: &App, change: impl FnOnce(&mut Reader)) {
    let Some(ui) = app.ui() else { return };
    let mail = ui.global::<Mail>();
    let mut reader = mail.get_reader();
    change(&mut reader);
    mail.set_reader(reader);
}

pub fn opened(app: &Rc<App>, token: u64, detail: Option<MessageDetail>) {
    if token != app.mail.open_token.get() {
        return;
    }
    let Some(ui) = app.ui() else { return };
    let Some(detail) = detail else {
        update_reader(app, |reader| reader.loading = false);
        return;
    };
    let rendered: Rendered = detail.body_html.as_deref().map(html::render).unwrap_or_default();
    let dark = app.config.borrow().dark;
    let blocks = body::blocks(&rendered, dark);
    let mut sources: HashMap<String, Vec<PictureSpot>> = HashMap::new();
    for (index, block) in rendered.blocks.iter().enumerate() {
        for (place, image) in block.images.iter().enumerate() {
            sources.entry(image.src.clone()).or_default().push((index, place, (image.width, image.height)));
        }
    }
    *app.mail.picture_blocks.borrow_mut() = sources;
    app.mail.blocks.set_vec(blocks);

    let attachments: Vec<AttachmentRow> = detail
        .attachments
        .iter()
        .filter(|attachment| !attachment.is_inline)
        .map(|attachment| AttachmentRow {
            id: attachment.id.as_str().into(),
            name: attachment.name.as_str().into(),
            size: format::size(attachment.size).into(),
        })
        .collect();
    ui.global::<Mail>().set_attachments(ModelRc::new(VecModel::from(attachments)));
    let unread = !detail.summary.is_read;
    update_reader(app, |reader| {
        reader.loading = false;
        reader.cc = format::names(&detail.cc).into();
        reader.remote_images = rendered.has_remote_images;
        reader.truncated = rendered.truncated;
        reader.plain = rendered.text.as_str().into();
        reader.categories = detail.categories.join(" · ").into();
        reader.unread = false;
    });
    let account = detail.summary.account.clone();
    let id = detail.summary.id.clone();
    *app.mail.detail.borrow_mut() = Some(detail);
    let original = app.config.borrow().original_view;
    if original && webview(app) {
        request_original(app);
    }
    if !(original && !webview(app) && open_page(app, false)) {
        request_pictures(app, app.mail.remote_shown.get());
    }
    if unread {
        app.engine.send(Request::SetRead { account, id, read: true });
    }
}

/// Draws the open message again, its colours suited to the new theme.
pub fn theme_changed(app: &Rc<App>) {
    let Some((account, id)) = app.mail.selected.borrow().clone() else { return };
    let token = app.token();
    app.mail.open_token.set(token);
    app.engine.send(Request::Open { account, id, token });
}

fn request_pictures(app: &App, remote: bool) {
    let Some((account, id)) = app.mail.selected.borrow().clone() else { return };
    let sources: Vec<String> = app.mail.picture_blocks.borrow().keys().cloned().collect();
    if sources.is_empty() {
        return;
    }
    app.engine.send(Request::Pictures { account, message: id, token: app.mail.open_token.get(), sources, remote });
}

pub fn pictures(app: &Rc<App>, token: u64, pictures: Vec<(String, SharedPixelBuffer<Rgba8Pixel>)>) {
    if token != app.mail.open_token.get() {
        return;
    }
    let places = app.mail.picture_blocks.borrow();
    for (source, buffer) in pictures {
        let Some(spots) = places.get(&source) else { continue };
        let natural = (buffer.width() as f32, buffer.height() as f32);
        let image = slint::Image::from_rgba8(buffer);
        for &(index, place, declared) in spots {
            let Some(mut block) = app.mail.blocks.row_data(index) else { continue };
            let mut row: Vec<crate::Picture> = block.pictures.iter().collect();
            let Some(picture) = row.get_mut(place) else { continue };
            let (width, height) = body::shown_size(declared, natural);
            picture.width = width;
            picture.height = height;
            picture.image = image.clone();
            picture.loaded = true;
            body::set_pictures(&mut block, row);
            app.mail.blocks.set_row_data(index, block);
        }
    }
}

fn summary_at(app: &App, index: i32) -> Option<MessageSummary> {
    (index >= 0).then(|| app.mail.list.summary(index as usize)).flatten()
}

fn toggle_flag(app: &Rc<App>, index: i32) {
    let targets = targets_at(app, index);
    let flag = if targets.iter().all(|m| m.flag == Flag::Flagged) { Flag::None } else { Flag::Flagged };
    change(app, targets, |_| Bulk::Flag(flag));
}

fn toggle_read(app: &Rc<App>, index: i32) {
    let targets = targets_at(app, index);
    let read = !targets.iter().all(|m| m.is_read);
    change(app, targets, |_| Bulk::Read(read));
}

/// Sends a change for `messages`: one request for a single message, a batch
/// per account for several.
fn change(app: &App, messages: Vec<MessageSummary>, what: impl Fn(&MessageSummary) -> Bulk) {
    if let [one] = messages.as_slice() {
        let (account, id) = (one.account.clone(), one.id.clone());
        let request = match what(one) {
            Bulk::Read(read) => Request::SetRead { account, id, read },
            Bulk::Flag(flag) => Request::SetFlag { account, id, flag },
            Bulk::Move(folder) => Request::Move { account, id, folder },
            Bulk::Delete => Request::Delete { account, id },
        };
        app.engine.send(request);
        return;
    }
    let mut by_account: HashMap<String, Vec<String>> = HashMap::new();
    let mut change = None;
    for message in &messages {
        change.get_or_insert_with(|| what(message));
        by_account.entry(message.account.clone()).or_default().push(message.id.clone());
    }
    let Some(change) = change else { return };
    for (account, ids) in by_account {
        app.engine.send(Request::Bulk { account, ids, change: change.clone() });
    }
}

/// Deletes the messages at `index`, or moves them to `destination`.
fn remove(app: &Rc<App>, index: i32, destination: Option<String>) {
    let targets = targets_at(app, index);
    let first = targets.iter().filter_map(|m| app.mail.list.index_of(&m.account, &m.id)).min();
    let open_leaves = targets
        .iter()
        .any(|m| app.mail.selected.borrow().as_ref().is_some_and(|(_, id)| *id == m.id));
    if open_leaves {
        *app.mail.selected.borrow_mut() = None;
        close_reader(app);
        app.mail.select_after.set(first);
    }
    change(app, targets, |_| match &destination {
        Some(folder) => Bulk::Move(folder.clone()),
        None => Bulk::Delete,
    });
}

/// Moves messages dragged onto a folder of `account`.
fn move_messages(app: &Rc<App>, messages: Vec<(String, String)>, account: &str, folder: &str) {
    if messages.iter().any(|(owner, _)| owner != account) {
        app.toast("Messages can only be moved within their own account");
        return;
    }
    let summaries: Vec<MessageSummary> = messages
        .iter()
        .filter_map(|(account, id)| app.store.message(account, id).ok().flatten().map(|d| d.summary))
        .filter(|summary| summary.folder_id != folder)
        .collect();
    if summaries.is_empty() {
        return;
    }
    let open_leaves = summaries
        .iter()
        .any(|m| app.mail.selected.borrow().as_ref().is_some_and(|(_, id)| *id == m.id));
    if open_leaves {
        let first = summaries.iter().filter_map(|m| app.mail.list.index_of(&m.account, &m.id)).min();
        *app.mail.selected.borrow_mut() = None;
        close_reader(app);
        app.mail.select_after.set(first);
    }
    change(app, summaries, |_| Bulk::Move(folder.to_owned()));
}

fn open_move(app: &Rc<App>, index: i32) {
    let targets = targets_at(app, index);
    show_move_dialog(app, targets);
}

fn show_move_dialog(app: &Rc<App>, messages: Vec<MessageSummary>) {
    let Some(ui) = app.ui() else { return };
    let Some(first) = messages.first() else { return };
    if messages.iter().any(|m| m.account != first.account) {
        app.toast("Messages can only be moved within their own account");
        return;
    }
    let targets: Vec<FolderRow> = folder_rows(app, &ui, Some(&first.account))
        .into_iter()
        .filter(|row| !row.header && (messages.len() > 1 || row.id.as_str() != first.folder_id))
        .collect();
    *app.mail.move_target.borrow_mut() = messages.iter().map(|m| (m.account.clone(), m.id.clone())).collect();
    let mail = ui.global::<Mail>();
    mail.set_move_targets(ModelRc::new(VecModel::from(targets)));
    mail.set_move_open(true);
}

fn move_to(app: &Rc<App>, folder: &str) {
    let messages = std::mem::take(&mut *app.mail.move_target.borrow_mut());
    let Some((account, _)) = messages.first().cloned() else { return };
    move_messages(app, messages, &account, folder);
}

/// An action from the panel shown for several selected messages.
fn bulk(app: &Rc<App>, action: &str) {
    let selection = app.mail.list.selection();
    if selection.is_empty() {
        return;
    }
    match action {
        "read" => change(app, selection, |_| Bulk::Read(true)),
        "unread" => change(app, selection, |_| Bulk::Read(false)),
        "flag" => {
            let flag = if selection.iter().all(|m| m.flag == Flag::Flagged) { Flag::None } else { Flag::Flagged };
            change(app, selection, |_| Bulk::Flag(flag));
        }
        "move" => show_move_dialog(app, selection),
        "archive" | "delete" => {
            let first = selection.iter().filter_map(|m| app.mail.list.index_of(&m.account, &m.id)).min();
            *app.mail.selected.borrow_mut() = None;
            close_reader(app);
            app.mail.select_after.set(first);
            let destination = (action == "archive").then(|| "archive".to_owned());
            change(app, selection, |_| match &destination {
                Some(folder) => Bulk::Move(folder.clone()),
                None => Bulk::Delete,
            });
        }
        _ => {}
    }
}

fn current_account(app: &App) -> String {
    let selected = app.mail.selected.borrow().as_ref().map(|(account, _)| account.clone());
    match (&*app.mail.view.borrow(), selected) {
        (View::Folder { account, .. }, _) => account.clone(),
        (_, Some(account)) => account,
        _ => app.accounts.borrow().first().map(|a| a.id.clone()).unwrap_or_default(),
    }
}

fn reset_compose(app: &App) {
    app.mail.attachments.borrow_mut().clear();
    *app.mail.reply.borrow_mut() = None;
    *app.mail.reply_draft.borrow_mut() = None;
    *app.mail.existing_draft.borrow_mut() = None;
    app.mail.reply_token.set(0);
    show_compose_attachments(app);
}

pub fn new_message(app: &Rc<App>, to: Vec<Address>, subject: String) {
    let Some(ui) = app.ui() else { return };
    reset_compose(app);
    let mail = ui.global::<Mail>();
    let account = current_account(app);
    mail.set_compose(Compose {
        open: true,
        account_index: app.account_index(&account),
        to: if to.is_empty() { SharedString::new() } else { (format::addresses(&to) + "; ").into() },
        subject: subject.into(),
        body: signature_for(app, &account).into(),
        ..Compose::default()
    });
    ui.global::<crate::Shell>().set_page(crate::Page::Mail);
    mail.set_focus_compose_request(mail.get_focus_compose_request() + 1);
}

/// Room to write, then the account's signature, if it has one.
fn signature_for(app: &App, account: &str) -> String {
    match app.config.borrow().signatures.get(account) {
        Some(signature) if !signature.trim().is_empty() => format!("\n\n{signature}"),
        _ => String::new(),
    }
}

fn reply(app: &Rc<App>, kind: i32) {
    let Some(ui) = app.ui() else { return };
    let Some((account, id)) = app.mail.selected.borrow().clone() else { return };
    let reply_kind = match kind {
        2 => ReplyKind::ReplyAll,
        3 => ReplyKind::Forward,
        _ => ReplyKind::Reply,
    };
    reset_compose(app);
    let detail = app.mail.detail.borrow().clone();
    let subject = detail.as_ref().map(|d| d.summary.subject.clone()).unwrap_or_default();
    let prefix = if reply_kind == ReplyKind::Forward { "FW: " } else { "RE: " };
    let subject = if subject.to_ascii_lowercase().starts_with(&prefix.to_ascii_lowercase()) {
        subject
    } else {
        format!("{prefix}{subject}")
    };
    // Shown at once; the server's draft fills in the exact recipients.
    let to = match (&detail, reply_kind) {
        (Some(detail), ReplyKind::Reply | ReplyKind::ReplyAll) => {
            let from = if detail.reply_to.is_empty() { vec![detail.summary.from.clone()] } else { detail.reply_to.clone() };
            format::addresses(&from)
        }
        _ => String::new(),
    };
    let token = app.token();
    app.mail.reply_token.set(token);
    *app.mail.reply.borrow_mut() = Some((reply_kind, id.clone()));
    let mail = ui.global::<Mail>();
    mail.set_compose(Compose {
        open: true,
        account_index: app.account_index(&account),
        to: to.into(),
        subject: subject.into(),
        quote: detail
            .as_ref()
            .map(|d| d.body_html.as_deref().map(|h| html::render(h).text).unwrap_or_else(|| d.summary.preview.clone()))
            .map(|text| text.chars().take(QUOTE_PREVIEW_CHARS).collect::<String>())
            .unwrap_or_default()
            .into(),
        kind,
        body: signature_for(app, &account).into(),
        ..Compose::default()
    });
    mail.set_focus_compose_request(mail.get_focus_compose_request() + 1);
    app.engine.send(Request::StartReply { account, id, kind: reply_kind, token });
}

pub fn reply_ready(app: &Rc<App>, token: u64, draft: wire::Message) {
    if token != app.mail.reply_token.get() {
        // The composer was closed or another reply started: drop the draft.
        return;
    }
    let Some(ui) = app.ui() else { return };
    let mail = ui.global::<Mail>();
    let mut compose = mail.get_compose();
    let addresses = |list: &Option<Vec<wire::Recipient>>| -> Vec<Address> {
        list.as_deref().unwrap_or_default().iter().map(Address::from).collect()
    };
    let to = addresses(&draft.to_recipients);
    let cc = addresses(&draft.cc_recipients);
    if !to.is_empty() {
        compose.to = (format::addresses(&to) + "; ").into();
    }
    if !cc.is_empty() {
        compose.cc = format::addresses(&cc).into();
        compose.show_cc = true;
    }
    mail.set_compose(compose);
    *app.mail.reply_draft.borrow_mut() = Some(draft);
}

fn edit_draft(app: &Rc<App>) {
    let Some(ui) = app.ui() else { return };
    let Some(detail) = app.mail.detail.borrow().clone() else { return };
    reset_compose(app);
    *app.mail.existing_draft.borrow_mut() = Some(detail.summary.id.clone());
    let text = detail.body_html.as_deref().map(|h| html::render(h).text).unwrap_or_default();
    let mail = ui.global::<Mail>();
    mail.set_compose(Compose {
        open: true,
        account_index: app.account_index(&detail.summary.account),
        to: format::addresses(&detail.summary.to).into(),
        cc: format::addresses(&detail.cc).into(),
        bcc: format::addresses(&detail.bcc).into(),
        show_cc: !detail.cc.is_empty() || !detail.bcc.is_empty(),
        subject: detail.summary.subject.as_str().into(),
        body: text.into(),
        important: detail.summary.high_importance,
        ..Compose::default()
    });
}

fn close_compose(app: &Rc<App>) {
    let Some(ui) = app.ui() else { return };
    reset_compose(app);
    let mail = ui.global::<Mail>();
    mail.set_compose(Compose::default());
    mail.set_suggestions(ModelRc::default());
    mail.set_suggestion_field(-1);
}

fn send(app: &Rc<App>, compose: Compose, draft_only: bool) {
    let Some(ui) = app.ui() else { return };
    let (to, bad_to) = format::parse_addresses(&compose.to);
    let (cc, bad_cc) = format::parse_addresses(&compose.cc);
    let (bcc, bad_bcc) = format::parse_addresses(&compose.bcc);
    let invalid: Vec<String> = bad_to.into_iter().chain(bad_cc).chain(bad_bcc).collect();
    if !invalid.is_empty() {
        app.toast(&format!("Not an address: {}", invalid.join(", ")));
        return;
    }
    if !draft_only && to.is_empty() && cc.is_empty() && bcc.is_empty() {
        app.toast("Add at least one recipient");
        return;
    }
    let Some(account) = app.account_at(compose.account_index) else { return };
    let draft = Draft {
        to,
        cc,
        bcc,
        subject: compose.subject.to_string(),
        body_html: html::markdown_to_html(&compose.body),
        attachments: app.mail.attachments.borrow().clone(),
        high_importance: compose.important,
        reply: app.mail.reply.borrow().clone(),
    };
    if draft_only {
        app.engine.send(Request::SaveDraft { account: account.id, draft });
        close_compose(app);
        return;
    }
    let mail = ui.global::<Mail>();
    let mut sending = compose;
    sending.sending = true;
    mail.set_compose(sending);
    app.engine.send(Request::Send {
        account: account.id,
        draft: Box::new(draft),
        reply: app.mail.reply_draft.borrow_mut().take().map(Box::new),
        existing: app.mail.existing_draft.borrow().clone(),
    });
}

pub fn sent(app: &Rc<App>) {
    close_compose(app);
}

pub fn send_failed(app: &Rc<App>, error: &str) {
    let Some(ui) = app.ui() else { return };
    let mail = ui.global::<Mail>();
    let mut compose = mail.get_compose();
    compose.sending = false;
    mail.set_compose(compose);
    app.toast(&format!("Not sent: {error}"));
}

fn attach(app: &Rc<App>) {
    let Some(paths) = rfd::FileDialog::new().set_title("Attach files").pick_files() else { return };
    for path in paths {
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "attachment".into());
        match std::fs::metadata(&path) {
            Ok(meta) if meta.len() > MAX_ATTACHMENT => {
                app.toast(&format!("{name} is larger than 150 MB"));
                continue;
            }
            Err(error) => {
                app.toast(&format!("Can't read {name}: {error}"));
                continue;
            }
            _ => {}
        }
        match std::fs::read(&path) {
            Ok(bytes) => app.mail.attachments.borrow_mut().push(NewAttachment {
                content_type: content_type(&name).into(),
                name,
                bytes,
            }),
            Err(error) => app.toast(&format!("Can't read {name}: {error}")),
        }
    }
    show_compose_attachments(app);
}

fn show_compose_attachments(app: &App) {
    let Some(ui) = app.ui() else { return };
    let rows: Vec<AttachmentRow> = app
        .mail
        .attachments
        .borrow()
        .iter()
        .enumerate()
        .map(|(index, attachment)| AttachmentRow {
            id: index.to_string().into(),
            name: attachment.name.as_str().into(),
            size: format::size(attachment.bytes.len() as u64).into(),
        })
        .collect();
    ui.global::<Mail>().set_compose_attachments(ModelRc::new(VecModel::from(rows)));
}

fn content_type(name: &str) -> &'static str {
    let extension = name.rsplit('.').next().unwrap_or_default().to_ascii_lowercase();
    match extension.as_str() {
        "pdf" => "application/pdf",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "txt" | "log" => "text/plain",
        "csv" => "text/csv",
        "html" | "htm" => "text/html",
        "zip" => "application/zip",
        "doc" => "application/msword",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "xls" => "application/vnd.ms-excel",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "ppt" => "application/vnd.ms-powerpoint",
        "pptx" => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        "ics" => "text/calendar",
        _ => "application/octet-stream",
    }
}

fn suggest(app: &Rc<App>, field: i32, text: &str) {
    let Some(ui) = app.ui() else { return };
    let mail = ui.global::<Mail>();
    let typed = format::typed_address(text);
    let found = if typed.chars().count() >= 2 { app.store.suggest_addresses(typed, SUGGESTIONS).unwrap_or_default() } else { Vec::new() };
    let labels: Vec<SharedString> = found.iter().map(|address| format::address(address).into()).collect();
    mail.set_suggestion_field(if labels.is_empty() { -1 } else { field });
    mail.set_suggestions(ModelRc::new(VecModel::from(labels)));
}

fn attachment(app: &Rc<App>, id: &str, open: bool) {
    let Some(detail) = app.mail.detail.borrow().clone() else { return };
    let Some(attachment) = detail.attachments.iter().find(|a| a.id == id) else { return };
    app.engine.send(Request::Attachment {
        account: detail.summary.account.clone(),
        message: detail.summary.id.clone(),
        attachment: attachment.id.clone(),
        name: attachment.name.clone(),
        open,
    });
}

/// Switches the reading pane between the message as sent and simplified.
/// As sent is drawn by Blitz, or WebView2 when comparing the two; off,
/// either is closed and its memory given back.
fn toggle_original(app: &Rc<App>) {
    let on = !app.config.borrow().original_view;
    app.config.borrow_mut().original_view = on;
    if let Err(error) = app.config.borrow().save() {
        log::warn!("saving settings: {error}");
    }
    if let Some(ui) = app.ui() {
        ui.global::<Mail>().set_original(on);
    }
    if webview(app) {
        if on {
            request_original(app);
        } else {
            *app.mail.original.loaded.borrow_mut() = None;
            #[cfg(windows)]
            app.mail.original.panel.borrow_mut().take();
        }
    } else if on {
        if open_page(app, false) {
            // The simplified view's pictures aren't needed meanwhile.
            for block in 0..app.mail.blocks.row_count() {
                if let Some(mut block_data) = app.mail.blocks.row_data(block)
                    && block_data.kind == 3
                {
                    let pictures: Vec<crate::Picture> = block_data
                        .pictures
                        .iter()
                        .map(|picture| crate::Picture { image: slint::Image::default(), loaded: false, ..picture })
                        .collect();
                    body::set_pictures(&mut block_data, pictures);
                    app.mail.blocks.set_row_data(block, block_data);
                }
            }
        }
    } else {
        close_page(app);
        request_pictures(app, app.mail.remote_shown.get());
    }
}

/// WebView2 shows messages as sent instead of Blitz, to compare them.
fn webview(app: &App) -> bool {
    cfg!(windows) && app.config.borrow().webview
}

/// Draws the open message as sent with Blitz. False if it can't be, and the
/// simplified view should show instead.
fn open_page(app: &Rc<App>, keep_scroll: bool) -> bool {
    let Some(ui) = app.ui() else { return false };
    let Some(html) = app.mail.detail.borrow().as_ref().and_then(|detail| detail.body_html.clone()) else {
        close_page(app);
        return false;
    };
    let Some((account, id)) = app.mail.selected.borrow().clone() else { return false };
    let resources = crate::page::Resources::new(app.engine.clone(), account, id, app.mail.remote_shown.get(), || {
        _ = slint::invoke_from_event_loop(|| crate::app::with_app(paint_page_loaded));
    });
    let Some(page) = crate::page::guard("reading a message", || crate::page::Page::new(&html, resources)) else {
        close_page(app);
        return false;
    };
    *app.mail.page.borrow_mut() = Some(page);
    let mail = ui.global::<Mail>();
    if !keep_scroll {
        let (width, height, _, _) = app.mail.page_view.get();
        app.mail.page_view.set((width, height, 0.0, 0.0));
        mail.set_page_width(0.0);
        mail.set_page_height(0.0);
    }
    mail.set_page(true);
    // A view already up paints the new page now; a new one asks when it
    // knows its size.
    paint_page(app);
    true
}

fn close_page(app: &App) {
    app.mail.page.borrow_mut().take();
    if let Some(ui) = app.ui() {
        let mail = ui.global::<Mail>();
        mail.set_page(false);
        mail.set_page_picture(slint::Image::default());
    }
}

/// Lays the page out for the pane and paints what is in view.
fn paint_page(app: &Rc<App>) {
    let Some(ui) = app.ui() else { return };
    let (width, height, x, y) = app.mail.page_view.get();
    if width <= 0.0 || height <= 0.0 {
        return;
    }
    let scale = ui.window().scale_factor();
    let painted = {
        let mut page = app.mail.page.borrow_mut();
        let Some(page) = page.as_mut() else { return };
        crate::page::guard("drawing a message", || {
            page.view(width, height, scale);
            (page.size(), page.paint(x, y))
        })
    };
    show_painted(app, painted);
}

/// Takes in what has arrived for the page, and paints it again.
fn paint_page_loaded(app: &Rc<App>) {
    let (width, height, x, y) = app.mail.page_view.get();
    let painted = {
        let mut page = app.mail.page.borrow_mut();
        let Some(page) = page.as_mut() else { return };
        crate::page::guard("loading a message's pictures", || {
            page.load();
            (width > 0.0 && height > 0.0).then(|| (page.size(), page.paint(x, y)))
        })
    };
    match painted {
        Some(None) => {}
        Some(Some(painted)) => show_painted(app, Some(painted)),
        None => show_painted(app, None),
    }
}

/// Shows a painted page, or the simplified view if Blitz failed.
fn show_painted(app: &Rc<App>, painted: Option<((f32, f32), slint::Image)>) {
    let Some(ui) = app.ui() else { return };
    let mail = ui.global::<Mail>();
    match painted {
        Some(((width, height), picture)) => {
            mail.set_page_width(width);
            mail.set_page_height(height);
            mail.set_page_picture(picture);
        }
        None => {
            drop(ui);
            close_page(app);
            app.status("This message can't be shown as sent, so it is simplified.");
            request_pictures(app, app.mail.remote_shown.get());
        }
    }
}

fn page_link(app: &App, x: f32, y: f32) -> Option<String> {
    let page = app.mail.page.borrow();
    let page = page.as_ref()?;
    crate::page::guard("finding a link", || page.link_at(x, y)).flatten()
}

fn request_original(app: &App) {
    let Some((account, id)) = app.mail.selected.borrow().clone() else { return };
    let token = app.token();
    app.mail.original.token.set(token);
    app.engine.send(Request::PrepareOriginal { account, id, remote: app.mail.remote_shown.get(), token });
}

pub fn original_ready(app: &Rc<App>, token: u64, path: PathBuf) {
    if token != app.mail.original.token.get() {
        return;
    }
    #[cfg(not(windows))]
    {
        if let Err(error) = crate::viewer::open_in_browser(&path) {
            app.toast(&format!("Can't open the original: {error}"));
        }
    }
    #[cfg(windows)]
    {
        *app.mail.original.page.borrow_mut() = Some(path);
        update_original(app);
    }
}

/// Puts the panel where the pane is, showing the open message's page, or
/// hides it while something covers the pane.
fn update_original(app: &App) {
    #[cfg(windows)]
    {
        let Some(ui) = app.ui() else { return };
        let state = &app.mail.original;
        let page = state.page.borrow().clone();
        let mut panel = state.panel.borrow_mut();
        let (Some(page), true) = (page, state.shown.get() && app.config.borrow().original_view) else {
            if let Some(panel) = panel.as_ref() {
                panel.set_visible(false);
            }
            return;
        };
        let panel = panel.get_or_insert_with(crate::viewer::Embedded::new);
        if state.loaded.borrow().as_ref() == Some(&page) {
            panel.set_bounds(state.area.get());
            panel.set_visible(true);
        } else {
            match panel.show(ui.window(), &page, state.area.get()) {
                Ok(()) => *state.loaded.borrow_mut() = Some(page),
                Err(error) => {
                    drop(ui);
                    log::warn!("WebView2: {error}");
                    app.status(&format!("Can't show the original: {error}"));
                }
            }
        }
    }
    #[cfg(not(windows))]
    let _ = app;
}

fn open_link(app: &Rc<App>, link: &str) {
    if let Some(rest) = link.strip_prefix("mailto:") {
        let (address, query) = rest.split_once('?').unwrap_or((rest, ""));
        let subject = query
            .split('&')
            .filter_map(|pair| pair.split_once('='))
            .find(|(key, _)| key.eq_ignore_ascii_case("subject"))
            .map(|(_, value)| url_decode(value))
            .unwrap_or_default();
        let (to, _) = format::parse_addresses(&url_decode(address));
        new_message(app, to, subject);
        return;
    }
    if let Err(error) = open::that_detached(link) {
        app.toast(&format!("Can't open the link: {error}"));
    }
}

fn url_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'%' if index + 2 < bytes.len() => {
                match u8::from_str_radix(std::str::from_utf8(&bytes[index + 1..index + 3]).unwrap_or(""), 16) {
                    Ok(value) => {
                        out.push(value);
                        index += 3;
                        continue;
                    }
                    Err(_) => out.push(b'%'),
                }
            }
            b'+' => out.push(b' '),
            byte => out.push(byte),
        }
        index += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

pub fn key(app: &Rc<App>, key: &str, control: bool, shift: bool) -> bool {
    let Some(ui) = app.ui() else { return false };
    let mail = ui.global::<Mail>();
    let selected = mail.get_selected();
    let count = app.mail.list.len() as i32;
    match (key.to_ascii_lowercase().as_str(), control, shift) {
        ("down", false, true) | ("up", false, true) if count > 0 => {
            // Extends the selection, as in the list Outlook keeps.
            let next = if key == "down" { (selected + 1).min(count - 1) } else { (selected - 1).max(0) };
            app.mail.list.select_range(next as usize);
            mail.set_selected(next);
            update_selection_count(app);
            true
        }
        ("down", false, false) => {
            if count > 0 {
                select(app, (selected + 1).min(count - 1));
            }
            true
        }
        ("up", false, false) => {
            if count > 0 {
                select(app, (selected - 1).max(0));
            }
            true
        }
        ("a", true, false) if count > 0 => {
            app.mail.list.select_all();
            update_selection_count(app);
            true
        }
        ("home", false, _) if count > 0 => {
            select(app, 0);
            true
        }
        ("end", false, _) if count > 0 => {
            select(app, count - 1);
            true
        }
        ("delete", false, _) if selected >= 0 => {
            remove(app, selected, None);
            true
        }
        ("n", true, false) => {
            new_message(app, Vec::new(), String::new());
            true
        }
        ("r", true, false) => {
            reply(app, 1);
            true
        }
        ("r", true, true) => {
            reply(app, 2);
            true
        }
        ("f", true, false) => {
            reply(app, 3);
            true
        }
        ("q", true, false) if selected >= 0 => {
            if let Some(summary) = summary_at(app, selected) {
                app.engine.send(Request::SetRead { account: summary.account, id: summary.id, read: true });
            }
            true
        }
        ("u", true, false) if selected >= 0 => {
            if let Some(summary) = summary_at(app, selected) {
                app.engine.send(Request::SetRead { account: summary.account, id: summary.id, read: false });
            }
            true
        }
        ("escape", false, false) if mail.get_compose().open && mail.get_compose().body.is_empty() => {
            close_compose(app);
            true
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mailto_parts_decode() {
        assert_eq!(url_decode("Hello%20there+you"), "Hello there you");
        assert_eq!(url_decode("100%"), "100%");
        assert_eq!(url_decode("caf%C3%A9"), "café");
    }
}
