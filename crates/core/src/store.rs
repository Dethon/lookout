//! The local cache: one SQLite file for every account.
//!
//! The engine writes through one connection; the interface reads through its
//! own, which WAL lets run alongside. Message bodies are only kept for
//! messages that were opened, and the oldest are dropped past a size cap.

use std::path::Path;

use rusqlite::{Connection, OptionalExtension, Row, params};

use crate::Result;
use crate::auth::Account;
use crate::model::{
    Address, Attachment, Attendee, Calendar, Contact, Event, Flag, Folder, MessageDetail,
    MessageSummary, Phone, Response,
};

const SCHEMA_VERSION: i64 = 2;
// SQLite's page cache per connection, in KiB (negative means KiB).
const CACHE_KIB: i64 = -2048;
// Bodies kept for messages opened earlier; the least recently opened go first.
const BODY_CAP_BYTES: i64 = 64 * 1024 * 1024;

const SCHEMA: &str = r#"
CREATE TABLE accounts (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    email TEXT NOT NULL,
    position INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE folders (
    account TEXT NOT NULL,
    id TEXT NOT NULL,
    parent_id TEXT,
    name TEXT NOT NULL,
    well_known TEXT,
    unread INTEGER NOT NULL DEFAULT 0,
    total INTEGER NOT NULL DEFAULT 0,
    position INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (account, id)
);
CREATE TABLE messages (
    account TEXT NOT NULL,
    id TEXT NOT NULL,
    folder_id TEXT NOT NULL,
    conversation_id TEXT NOT NULL DEFAULT '',
    subject TEXT NOT NULL DEFAULT '',
    from_name TEXT NOT NULL DEFAULT '',
    from_address TEXT NOT NULL DEFAULT '',
    to_json TEXT NOT NULL DEFAULT '[]',
    cc_json TEXT NOT NULL DEFAULT '[]',
    reply_to_json TEXT NOT NULL DEFAULT '[]',
    preview TEXT NOT NULL DEFAULT '',
    received INTEGER NOT NULL DEFAULT 0,
    sent INTEGER NOT NULL DEFAULT 0,
    is_read INTEGER NOT NULL DEFAULT 0,
    is_draft INTEGER NOT NULL DEFAULT 0,
    flag INTEGER NOT NULL DEFAULT 0,
    high_importance INTEGER NOT NULL DEFAULT 0,
    has_attachments INTEGER NOT NULL DEFAULT 0,
    categories TEXT NOT NULL DEFAULT '',
    body_html TEXT,
    body_text TEXT NOT NULL DEFAULT '',
    attachments_json TEXT,
    opened INTEGER NOT NULL DEFAULT 0,
    UNIQUE (account, id)
);
CREATE INDEX messages_by_folder ON messages (account, folder_id, received DESC);
CREATE INDEX messages_opened ON messages (opened) WHERE body_html IS NOT NULL;
CREATE VIRTUAL TABLE messages_fts USING fts5 (
    subject, from_name, from_address, preview, body_text,
    content = 'messages', content_rowid = 'rowid',
    tokenize = 'unicode61 remove_diacritics 2'
);
CREATE TRIGGER messages_ai AFTER INSERT ON messages BEGIN
    INSERT INTO messages_fts (rowid, subject, from_name, from_address, preview, body_text)
    VALUES (new.rowid, new.subject, new.from_name, new.from_address, new.preview, new.body_text);
END;
CREATE TRIGGER messages_ad AFTER DELETE ON messages BEGIN
    INSERT INTO messages_fts (messages_fts, rowid, subject, from_name, from_address, preview, body_text)
    VALUES ('delete', old.rowid, old.subject, old.from_name, old.from_address, old.preview, old.body_text);
END;
CREATE TRIGGER messages_au AFTER UPDATE OF subject, from_name, from_address, preview, body_text ON messages BEGIN
    INSERT INTO messages_fts (messages_fts, rowid, subject, from_name, from_address, preview, body_text)
    VALUES ('delete', old.rowid, old.subject, old.from_name, old.from_address, old.preview, old.body_text);
    INSERT INTO messages_fts (rowid, subject, from_name, from_address, preview, body_text)
    VALUES (new.rowid, new.subject, new.from_name, new.from_address, new.preview, new.body_text);
END;
CREATE TABLE sync_state (
    account TEXT NOT NULL,
    key TEXT NOT NULL,
    link TEXT NOT NULL,
    PRIMARY KEY (account, key)
);
CREATE TABLE calendars (
    account TEXT NOT NULL,
    id TEXT NOT NULL,
    name TEXT NOT NULL,
    color TEXT NOT NULL DEFAULT '',
    can_edit INTEGER NOT NULL DEFAULT 0,
    is_default INTEGER NOT NULL DEFAULT 0,
    shown INTEGER NOT NULL DEFAULT 1,
    PRIMARY KEY (account, id)
);
CREATE TABLE events (
    account TEXT NOT NULL,
    id TEXT NOT NULL,
    calendar_id TEXT NOT NULL,
    subject TEXT NOT NULL DEFAULT '',
    start INTEGER NOT NULL,
    end INTEGER NOT NULL,
    all_day INTEGER NOT NULL DEFAULT 0,
    location TEXT NOT NULL DEFAULT '',
    organizer_json TEXT NOT NULL DEFAULT '{}',
    attendees_json TEXT NOT NULL DEFAULT '[]',
    preview TEXT NOT NULL DEFAULT '',
    body_html TEXT NOT NULL DEFAULT '',
    response TEXT NOT NULL DEFAULT 'none',
    show_as TEXT NOT NULL DEFAULT 'busy',
    is_cancelled INTEGER NOT NULL DEFAULT 0,
    is_online INTEGER NOT NULL DEFAULT 0,
    join_url TEXT NOT NULL DEFAULT '',
    is_recurring INTEGER NOT NULL DEFAULT 0,
    web_link TEXT NOT NULL DEFAULT '',
    PRIMARY KEY (account, id)
);
CREATE INDEX events_by_time ON events (start, end);
CREATE TABLE contacts (
    account TEXT NOT NULL,
    id TEXT NOT NULL,
    display_name TEXT NOT NULL DEFAULT '',
    given_name TEXT NOT NULL DEFAULT '',
    surname TEXT NOT NULL DEFAULT '',
    emails_json TEXT NOT NULL DEFAULT '[]',
    phones_json TEXT NOT NULL DEFAULT '[]',
    company TEXT NOT NULL DEFAULT '',
    job_title TEXT NOT NULL DEFAULT '',
    notes TEXT NOT NULL DEFAULT '',
    PRIMARY KEY (account, id)
);
"#;

// What each version adds to the one before. Caches made earlier run those
// they lack; new ones run `SCHEMA` (version 1), then all of them. Index 0
// takes version 1 to 2.
const MIGRATIONS: &[&str] = &[PHOTOS];

const PHOTOS: &str = r#"
CREATE TABLE IF NOT EXISTS photos (
    account TEXT NOT NULL,
    contact_id TEXT NOT NULL,
    -- NULL when the contact has no picture.
    data BLOB,
    checked INTEGER NOT NULL,
    PRIMARY KEY (account, contact_id)
);
"#;

pub struct Store {
    db: Connection,
}

/// A row's place in the cache: what the message list keeps per message.
pub type RowId = i64;

const SUMMARY_COLUMNS: &str = "account, id, folder_id, conversation_id, subject, from_name, \
    from_address, to_json, preview, received, is_read, is_draft, flag, high_importance, has_attachments";

fn summary_from(row: &Row) -> rusqlite::Result<MessageSummary> {
    Ok(MessageSummary {
        account: row.get(0)?,
        id: row.get(1)?,
        folder_id: row.get(2)?,
        conversation_id: row.get(3)?,
        subject: row.get(4)?,
        from: Address { name: row.get(5)?, address: row.get(6)? },
        to: json_or_default(&row.get::<_, String>(7)?),
        preview: row.get(8)?,
        received: row.get(9)?,
        is_read: row.get(10)?,
        is_draft: row.get(11)?,
        flag: Flag::from_i64(row.get(12)?),
        high_importance: row.get(13)?,
        has_attachments: row.get(14)?,
    })
}

fn json_or_default<T: serde::de::DeserializeOwned + Default>(text: &str) -> T {
    serde_json::from_str(text).unwrap_or_default()
}

fn to_json<T: serde::Serialize>(value: &T) -> String {
    serde_json::to_string(value).unwrap_or_default()
}

/// A message as written by the sync: everything but the body.
#[derive(Debug, Clone)]
pub struct MessageRecord {
    pub summary: MessageSummary,
    pub cc: Vec<Address>,
    pub reply_to: Vec<Address>,
    pub sent: i64,
    pub categories: Vec<String>,
}

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let db = Connection::open(path)?;
        Self::prepare(db)
    }

    pub fn open_in_memory() -> Result<Self> {
        Self::prepare(Connection::open_in_memory()?)
    }

    fn prepare(db: Connection) -> Result<Self> {
        db.pragma_update(None, "journal_mode", "WAL")?;
        db.pragma_update(None, "synchronous", "NORMAL")?;
        db.pragma_update(None, "foreign_keys", "OFF")?;
        db.pragma_update(None, "cache_size", CACHE_KIB)?;
        db.pragma_update(None, "temp_store", "MEMORY")?;
        db.busy_timeout(std::time::Duration::from_secs(5))?;
        let version: i64 = db.pragma_query_value(None, "user_version", |row| row.get(0))?;
        if version == 0 {
            db.execute_batch(SCHEMA)?;
            for migration in MIGRATIONS {
                db.execute_batch(migration)?;
            }
        } else {
            for migration in MIGRATIONS.iter().skip((version - 1).max(0) as usize) {
                db.execute_batch(migration)?;
            }
        }
        if version != SCHEMA_VERSION {
            db.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        }
        Ok(Self { db })
    }

    /// The cache file in the data folder.
    pub fn default_path() -> Result<std::path::PathBuf> {
        Ok(crate::config::data_dir()?.join("cache.sqlite3"))
    }

    // Accounts

    pub fn accounts(&self) -> Result<Vec<Account>> {
        let mut statement = self.db.prepare_cached("SELECT id, name, email FROM accounts ORDER BY position, rowid")?;
        let rows = statement.query_map([], |row| {
            Ok(Account { id: row.get(0)?, name: row.get(1)?, email: row.get(2)? })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn put_account(&self, account: &Account) -> Result<()> {
        self.db.execute(
            "INSERT INTO accounts (id, name, email, position)
             VALUES (?1, ?2, ?3, (SELECT COALESCE(MAX(position), 0) + 1 FROM accounts))
             ON CONFLICT (id) DO UPDATE SET name = excluded.name, email = excluded.email",
            params![account.id, account.name, account.email],
        )?;
        Ok(())
    }

    /// Forgets an account and everything cached for it.
    pub fn remove_account(&mut self, account: &str) -> Result<()> {
        let tx = self.db.transaction()?;
        for table in ["accounts", "folders", "messages", "sync_state", "calendars", "events", "contacts", "photos"] {
            let column = if table == "accounts" { "id" } else { "account" };
            tx.execute(&format!("DELETE FROM {table} WHERE {column} = ?1"), [account])?;
        }
        tx.commit()?;
        Ok(())
    }

    // Sync state

    pub fn sync_link(&self, account: &str, key: &str) -> Result<Option<String>> {
        Ok(self
            .db
            .query_row(
                "SELECT link FROM sync_state WHERE account = ?1 AND key = ?2",
                [account, key],
                |row| row.get(0),
            )
            .optional()?)
    }

    pub fn set_sync_link(&self, account: &str, key: &str, link: Option<&str>) -> Result<()> {
        match link {
            Some(link) => self.db.execute(
                "INSERT INTO sync_state (account, key, link) VALUES (?1, ?2, ?3)
                 ON CONFLICT (account, key) DO UPDATE SET link = excluded.link",
                [account, key, link],
            )?,
            None => self.db.execute("DELETE FROM sync_state WHERE account = ?1 AND key = ?2", [account, key])?,
        };
        Ok(())
    }

    // Folders

    /// Replaces the account's folder list, in the order given.
    pub fn put_folders(&mut self, account: &str, folders: &[Folder]) -> Result<()> {
        let tx = self.db.transaction()?;
        tx.execute("DELETE FROM folders WHERE account = ?1", [account])?;
        {
            let mut insert = tx.prepare_cached(
                "INSERT INTO folders (account, id, parent_id, name, well_known, unread, total, position)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            )?;
            for (position, folder) in folders.iter().enumerate() {
                insert.execute(params![
                    account,
                    folder.id,
                    folder.parent_id,
                    folder.name,
                    folder.well_known,
                    folder.unread,
                    folder.total,
                    position as i64
                ])?;
            }
        }
        // Messages of folders that are gone went with them.
        tx.execute(
            "DELETE FROM messages WHERE account = ?1 AND folder_id NOT IN (SELECT id FROM folders WHERE account = ?1)",
            [account],
        )?;
        tx.execute(
            "DELETE FROM sync_state WHERE account = ?1 AND key LIKE 'messages:%'
             AND substr(key, 10) NOT IN (SELECT id FROM folders WHERE account = ?1)",
            [account],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn folders(&self, account: &str) -> Result<Vec<Folder>> {
        let mut statement = self.db.prepare_cached(
            "SELECT id, parent_id, name, well_known, unread, total FROM folders
             WHERE account = ?1 ORDER BY position",
        )?;
        let rows = statement.query_map([account], |row| {
            Ok(Folder {
                id: row.get(0)?,
                parent_id: row.get(1)?,
                name: row.get(2)?,
                well_known: row.get(3)?,
                unread: row.get(4)?,
                total: row.get(5)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn well_known_folder(&self, account: &str, name: &str) -> Result<Option<String>> {
        Ok(self
            .db
            .query_row(
                "SELECT id FROM folders WHERE account = ?1 AND well_known = ?2",
                [account, name],
                |row| row.get(0),
            )
            .optional()?)
    }

    /// Shifts a folder's unread and total counts after a local change, until
    /// the next sync brings the server's.
    pub fn adjust_counts(&self, account: &str, folder: &str, unread: i64, total: i64) -> Result<()> {
        self.db.execute(
            "UPDATE folders SET unread = MAX(0, unread + ?3), total = MAX(0, total + ?4)
             WHERE account = ?1 AND id = ?2",
            params![account, folder, unread, total],
        )?;
        Ok(())
    }

    // Messages

    /// Adds or updates messages from the server, keeping cached bodies.
    pub fn put_messages(&mut self, records: &[MessageRecord]) -> Result<()> {
        let tx = self.db.transaction()?;
        {
            let mut upsert = tx.prepare_cached(
                "INSERT INTO messages (account, id, folder_id, conversation_id, subject, from_name,
                    from_address, to_json, cc_json, reply_to_json, preview, received, sent, is_read,
                    is_draft, flag, high_importance, has_attachments, categories)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19)
                 ON CONFLICT (account, id) DO UPDATE SET
                    folder_id = excluded.folder_id, conversation_id = excluded.conversation_id,
                    subject = excluded.subject, from_name = excluded.from_name,
                    from_address = excluded.from_address, to_json = excluded.to_json,
                    cc_json = excluded.cc_json, reply_to_json = excluded.reply_to_json,
                    preview = excluded.preview, received = excluded.received, sent = excluded.sent,
                    is_read = excluded.is_read, is_draft = excluded.is_draft, flag = excluded.flag,
                    high_importance = excluded.high_importance,
                    has_attachments = excluded.has_attachments, categories = excluded.categories",
            )?;
            for record in records {
                let summary = &record.summary;
                upsert.execute(params![
                    summary.account,
                    summary.id,
                    summary.folder_id,
                    summary.conversation_id,
                    summary.subject,
                    summary.from.name,
                    summary.from.address,
                    to_json(&summary.to),
                    to_json(&record.cc),
                    to_json(&record.reply_to),
                    summary.preview,
                    summary.received,
                    record.sent,
                    summary.is_read,
                    summary.is_draft,
                    summary.flag.to_i64(),
                    summary.high_importance,
                    summary.has_attachments,
                    record.categories.join("\u{1f}"),
                ])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub fn remove_messages(&mut self, account: &str, ids: &[String]) -> Result<()> {
        let tx = self.db.transaction()?;
        {
            let mut delete = tx.prepare_cached("DELETE FROM messages WHERE account = ?1 AND id = ?2")?;
            for id in ids {
                delete.execute([account, id])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Drops a folder's messages, before syncing it again from scratch.
    pub fn clear_folder(&self, account: &str, folder: &str) -> Result<()> {
        self.db.execute("DELETE FROM messages WHERE account = ?1 AND folder_id = ?2", [account, folder])?;
        Ok(())
    }

    /// The rows of a folder, newest first; only unread ones if asked.
    pub fn folder_rows(&self, account: &str, folder: &str, unread_only: bool) -> Result<Vec<RowId>> {
        let mut statement = self.db.prepare_cached(
            "SELECT rowid FROM messages WHERE account = ?1 AND folder_id = ?2 AND (NOT ?3 OR NOT is_read)
             ORDER BY received DESC",
        )?;
        let rows = statement.query_map(params![account, folder, unread_only], |row| row.get(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Rows matching a search, newest first, in one account or all of them.
    /// Every word must match the start of a word in the message.
    pub fn search_rows(&self, account: Option<&str>, query: &str, limit: usize) -> Result<Vec<RowId>> {
        let Some(expression) = fts_query(query) else {
            return Ok(Vec::new());
        };
        let mut statement = self.db.prepare_cached(
            "SELECT m.rowid FROM messages_fts f JOIN messages m ON m.rowid = f.rowid
             WHERE messages_fts MATCH ?1 AND (?2 IS NULL OR m.account = ?2)
             ORDER BY m.received DESC LIMIT ?3",
        )?;
        let rows = statement.query_map(params![expression, account, limit as i64], |row| row.get(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn summary(&self, row: RowId) -> Result<Option<MessageSummary>> {
        let mut statement =
            self.db.prepare_cached(&format!("SELECT {SUMMARY_COLUMNS} FROM messages WHERE rowid = ?1"))?;
        Ok(statement.query_row([row], summary_from).optional()?)
    }

    pub fn folder_of(&self, account: &str, id: &str) -> Result<Option<String>> {
        Ok(self
            .db
            .query_row("SELECT folder_id FROM messages WHERE account = ?1 AND id = ?2", [account, id], |row| row.get(0))
            .optional()?)
    }

    pub fn row_of(&self, account: &str, id: &str) -> Result<Option<RowId>> {
        Ok(self
            .db
            .query_row("SELECT rowid FROM messages WHERE account = ?1 AND id = ?2", [account, id], |row| row.get(0))
            .optional()?)
    }

    pub fn message(&self, account: &str, id: &str) -> Result<Option<MessageDetail>> {
        let mut statement = self.db.prepare_cached(&format!(
            "SELECT {SUMMARY_COLUMNS}, cc_json, reply_to_json, sent, categories, body_html, attachments_json
             FROM messages WHERE account = ?1 AND id = ?2"
        ))?;
        Ok(statement
            .query_row([account, id], |row| {
                let categories: String = row.get(18)?;
                Ok(MessageDetail {
                    summary: summary_from(row)?,
                    cc: json_or_default(&row.get::<_, String>(15)?),
                    bcc: Vec::new(),
                    reply_to: json_or_default(&row.get::<_, String>(16)?),
                    sent: row.get(17)?,
                    categories: categories.split('\u{1f}').filter(|c| !c.is_empty()).map(str::to_owned).collect(),
                    body_html: row.get(19)?,
                    attachments: row
                        .get::<_, Option<String>>(20)?
                        .map(|json| json_or_default(&json))
                        .unwrap_or_default(),
                })
            })
            .optional()?)
    }

    /// Keeps a fetched body, its text for searching and its attachments.
    pub fn put_body(
        &self,
        account: &str,
        id: &str,
        html: &str,
        text: &str,
        attachments: &[Attachment],
    ) -> Result<()> {
        self.db.execute(
            "UPDATE messages SET body_html = ?3, body_text = ?4, attachments_json = ?5, opened = ?6
             WHERE account = ?1 AND id = ?2",
            params![account, id, html, text, to_json(&attachments), now()],
        )?;
        Ok(())
    }

    /// Marks a cached body as just read, so it is the last to be dropped.
    pub fn touch_body(&self, account: &str, id: &str) -> Result<()> {
        self.db.execute(
            "UPDATE messages SET opened = ?3 WHERE account = ?1 AND id = ?2",
            params![account, id, now()],
        )?;
        Ok(())
    }

    /// Drops the least recently opened bodies past the cap. Their text stays
    /// searchable.
    pub fn trim_bodies(&self) -> Result<()> {
        let total: i64 = self.db.query_row(
            "SELECT COALESCE(SUM(LENGTH(body_html)), 0) FROM messages WHERE body_html IS NOT NULL",
            [],
            |row| row.get(0),
        )?;
        if total <= BODY_CAP_BYTES {
            return Ok(());
        }
        let mut excess = total - BODY_CAP_BYTES * 3 / 4;
        let mut statement = self.db.prepare(
            "SELECT rowid, LENGTH(body_html) FROM messages WHERE body_html IS NOT NULL ORDER BY opened",
        )?;
        let mut drop = Vec::new();
        let mut rows = statement.query([])?;
        while let Some(row) = rows.next()?
            && excess > 0
        {
            drop.push(row.get::<_, i64>(0)?);
            excess -= row.get::<_, i64>(1)?;
        }
        for row in drop {
            self.db.execute("UPDATE messages SET body_html = NULL WHERE rowid = ?1", [row])?;
        }
        Ok(())
    }

    pub fn set_read(&self, account: &str, id: &str, read: bool) -> Result<()> {
        self.db.execute(
            "UPDATE messages SET is_read = ?3 WHERE account = ?1 AND id = ?2",
            params![account, id, read],
        )?;
        Ok(())
    }

    pub fn set_flag(&self, account: &str, id: &str, flag: Flag) -> Result<()> {
        self.db.execute(
            "UPDATE messages SET flag = ?3 WHERE account = ?1 AND id = ?2",
            params![account, id, flag.to_i64()],
        )?;
        Ok(())
    }

    pub fn set_folder(&self, account: &str, id: &str, folder: &str) -> Result<()> {
        self.db.execute(
            "UPDATE messages SET folder_id = ?3 WHERE account = ?1 AND id = ?2",
            params![account, id, folder],
        )?;
        Ok(())
    }

    // Calendars

    pub fn put_calendars(&mut self, account: &str, calendars: &[Calendar]) -> Result<()> {
        let tx = self.db.transaction()?;
        tx.execute(
            "DELETE FROM calendars WHERE account = ?1 AND id NOT IN (SELECT value FROM json_each(?2))",
            params![account, to_json(&calendars.iter().map(|c| &c.id).collect::<Vec<_>>())],
        )?;
        {
            let mut upsert = tx.prepare_cached(
                "INSERT INTO calendars (account, id, name, color, can_edit, is_default)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT (account, id) DO UPDATE SET name = excluded.name, color = excluded.color,
                    can_edit = excluded.can_edit, is_default = excluded.is_default",
            )?;
            for calendar in calendars {
                upsert.execute(params![
                    account,
                    calendar.id,
                    calendar.name,
                    calendar.color,
                    calendar.can_edit,
                    calendar.is_default
                ])?;
            }
        }
        tx.execute(
            "DELETE FROM events WHERE account = ?1 AND calendar_id NOT IN
             (SELECT id FROM calendars WHERE account = ?1)",
            [account],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Every account's calendars, with whether each is shown.
    pub fn calendars(&self) -> Result<Vec<(String, Calendar, bool)>> {
        let mut statement = self.db.prepare_cached(
            "SELECT c.account, c.id, c.name, c.color, c.can_edit, c.is_default, c.shown
             FROM calendars c JOIN accounts a ON a.id = c.account
             ORDER BY a.position, c.is_default DESC, c.name",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get(0)?,
                Calendar {
                    id: row.get(1)?,
                    name: row.get(2)?,
                    color: row.get(3)?,
                    can_edit: row.get(4)?,
                    is_default: row.get(5)?,
                },
                row.get(6)?,
            ))
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn set_calendar_shown(&self, account: &str, id: &str, shown: bool) -> Result<()> {
        self.db.execute(
            "UPDATE calendars SET shown = ?3 WHERE account = ?1 AND id = ?2",
            params![account, id, shown],
        )?;
        Ok(())
    }

    pub fn put_events(&mut self, events: &[Event]) -> Result<()> {
        let tx = self.db.transaction()?;
        {
            let mut upsert = tx.prepare_cached(
                "INSERT OR REPLACE INTO events (account, id, calendar_id, subject, start, end, all_day,
                    location, organizer_json, attendees_json, preview, body_html, response, show_as,
                    is_cancelled, is_online, join_url, is_recurring, web_link)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19)",
            )?;
            for event in events {
                upsert.execute(params![
                    event.account,
                    event.id,
                    event.calendar_id,
                    event.subject,
                    event.start,
                    event.end,
                    event.all_day,
                    event.location,
                    to_json(&event.organizer),
                    to_json(&event.attendees),
                    event.preview,
                    event.body_html,
                    event.response.as_str(),
                    event.show_as,
                    event.is_cancelled,
                    event.is_online,
                    event.join_url,
                    event.is_recurring,
                    event.web_link,
                ])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub fn remove_events(&mut self, account: &str, ids: &[String]) -> Result<()> {
        let tx = self.db.transaction()?;
        {
            let mut delete = tx.prepare_cached("DELETE FROM events WHERE account = ?1 AND id = ?2")?;
            for id in ids {
                delete.execute([account, id])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub fn clear_events(&self, account: &str) -> Result<()> {
        self.db.execute("DELETE FROM events WHERE account = ?1", [account])?;
        Ok(())
    }

    /// Events of shown calendars overlapping `[start, end)`, by start.
    /// All-day events are compared by their date, which is kept in UTC.
    pub fn events_between(&self, start: i64, end: i64) -> Result<Vec<Event>> {
        // An all-day event's stored UTC midnight can be up to 14 hours from
        // the local midnight it stands for.
        const ZONE_SLACK: i64 = 14 * 3600;
        let mut statement = self.db.prepare_cached(
            "SELECT e.account, e.id, e.calendar_id, e.subject, e.start, e.end, e.all_day, e.location,
                    e.organizer_json, e.attendees_json, e.preview, e.body_html, e.response, e.show_as,
                    e.is_cancelled, e.is_online, e.join_url, e.is_recurring, e.web_link
             FROM events e JOIN calendars c ON c.account = e.account AND c.id = e.calendar_id
             WHERE c.shown AND e.start < ?2 + ?3 AND e.end > ?1 - ?3
             ORDER BY e.all_day DESC, e.start",
        )?;
        let rows = statement.query_map(params![start, end, ZONE_SLACK], event_from)?;
        let events: Vec<Event> = rows.collect::<rusqlite::Result<_>>()?;
        Ok(events
            .into_iter()
            .filter(|event| {
                if event.all_day {
                    let first = crate::when::local_midnight(crate::when::floating_date(event.start));
                    let last = crate::when::local_midnight(crate::when::floating_date(event.end));
                    first < end && last > start
                } else {
                    event.start < end && event.end > start
                }
            })
            .collect())
    }

    /// The ids of a calendar's events overlapping `[start, end)`, shown or not.
    pub fn event_ids_in(&self, account: &str, calendar: &str, start: i64, end: i64) -> Result<Vec<String>> {
        // All-day events are kept at UTC midnight; allow for the zone.
        const ZONE_SLACK: i64 = 14 * 3600;
        let mut statement = self.db.prepare_cached(
            "SELECT id FROM events WHERE account = ?1 AND calendar_id = ?2 AND start < ?4 + ?5 AND end > ?3 - ?5",
        )?;
        let rows = statement.query_map(params![account, calendar, start, end, ZONE_SLACK], |row| row.get(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn event(&self, account: &str, id: &str) -> Result<Option<Event>> {
        let mut statement = self.db.prepare_cached(
            "SELECT account, id, calendar_id, subject, start, end, all_day, location, organizer_json,
                    attendees_json, preview, body_html, response, show_as, is_cancelled, is_online,
                    join_url, is_recurring, web_link
             FROM events WHERE account = ?1 AND id = ?2",
        )?;
        Ok(statement.query_row([account, id], event_from).optional()?)
    }

    // Contacts

    pub fn put_contacts(&mut self, contacts: &[Contact]) -> Result<()> {
        let tx = self.db.transaction()?;
        {
            let mut upsert = tx.prepare_cached(
                "INSERT OR REPLACE INTO contacts (account, id, display_name, given_name, surname,
                    emails_json, phones_json, company, job_title, notes)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            )?;
            for contact in contacts {
                upsert.execute(params![
                    contact.account,
                    contact.id,
                    contact.display_name,
                    contact.given_name,
                    contact.surname,
                    to_json(&contact.emails),
                    to_json(&contact.phones),
                    contact.company,
                    contact.job_title,
                    contact.notes,
                ])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub fn remove_contacts(&mut self, account: &str, ids: &[String]) -> Result<()> {
        let tx = self.db.transaction()?;
        {
            let mut delete = tx.prepare_cached("DELETE FROM contacts WHERE account = ?1 AND id = ?2")?;
            for id in ids {
                delete.execute([account, id])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub fn clear_contacts(&self, account: &str) -> Result<()> {
        self.db.execute("DELETE FROM contacts WHERE account = ?1", [account])?;
        Ok(())
    }

    /// Every account's contacts by name, or those matching `filter`.
    pub fn contacts(&self, filter: &str) -> Result<Vec<Contact>> {
        let pattern = format!("%{}%", filter.trim().replace(['%', '_'], ""));
        let mut statement = self.db.prepare_cached(
            "SELECT account, id, display_name, given_name, surname, emails_json, phones_json,
                    company, job_title, notes
             FROM contacts
             WHERE display_name LIKE ?1 OR emails_json LIKE ?1 OR company LIKE ?1
             ORDER BY display_name COLLATE NOCASE",
        )?;
        let rows = statement.query_map([pattern], contact_from)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn contact(&self, account: &str, id: &str) -> Result<Option<Contact>> {
        let mut statement = self.db.prepare_cached(
            "SELECT account, id, display_name, given_name, surname, emails_json, phones_json,
                    company, job_title, notes
             FROM contacts WHERE account = ?1 AND id = ?2",
        )?;
        Ok(statement.query_row([account, id], contact_from).optional()?)
    }

    /// The latest messages from or to any of these addresses.
    pub fn messages_with(&self, addresses: &[String], limit: usize) -> Result<Vec<MessageSummary>> {
        if addresses.is_empty() {
            return Ok(Vec::new());
        }
        let mut conditions = Vec::new();
        let mut values: Vec<String> = Vec::new();
        for address in addresses {
            values.push(address.trim().to_owned());
            conditions.push(format!("from_address LIKE ?{}", values.len()));
            // Quoted, so a longer address holding this one doesn't match.
            values.push(format!("%\"{}\"%", address.trim().replace(['%', '_'], "")));
            conditions.push(format!("to_json LIKE ?{}", values.len()));
        }
        let sql = format!(
            "SELECT {SUMMARY_COLUMNS} FROM messages WHERE is_draft = 0 AND ({}) ORDER BY received DESC LIMIT {limit}",
            conditions.join(" OR ")
        );
        let mut statement = self.db.prepare(&sql)?;
        let rows = statement.query_map(rusqlite::params_from_iter(values.iter()), summary_from)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    // Contacts' pictures

    /// Contacts whose picture wasn't looked for since `before`, never
    /// looked for first.
    pub fn contacts_without_photos(&self, account: &str, before: i64, limit: usize) -> Result<Vec<String>> {
        let mut statement = self.db.prepare_cached(
            "SELECT c.id FROM contacts c
             LEFT JOIN photos p ON p.account = c.account AND p.contact_id = c.id
             WHERE c.account = ?1 AND (p.checked IS NULL OR p.checked < ?2)
             ORDER BY p.checked IS NOT NULL, p.checked LIMIT ?3",
        )?;
        let rows = statement.query_map(params![account, before, limit as i64], |row| row.get(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Keeps a contact's picture, or that it has none.
    pub fn put_photo(&self, account: &str, contact: &str, data: Option<&[u8]>, checked: i64) -> Result<()> {
        self.db.execute(
            "INSERT OR REPLACE INTO photos (account, contact_id, data, checked) VALUES (?1, ?2, ?3, ?4)",
            params![account, contact, data, checked],
        )?;
        Ok(())
    }

    /// Forgets the pictures of contacts that are gone.
    pub fn drop_orphan_photos(&self) -> Result<()> {
        self.db.execute(
            "DELETE FROM photos WHERE NOT EXISTS
             (SELECT 1 FROM contacts c WHERE c.account = photos.account AND c.id = photos.contact_id)",
            [],
        )?;
        Ok(())
    }

    pub fn contact_photo(&self, account: &str, contact: &str) -> Result<Option<Vec<u8>>> {
        let mut statement = self
            .db
            .prepare_cached("SELECT data FROM photos WHERE account = ?1 AND contact_id = ?2 AND data IS NOT NULL")?;
        Ok(statement.query_row([account, contact], |row| row.get(0)).optional()?)
    }

    /// Every contact's addresses, lowercased, with the contact's account and
    /// id.
    pub fn contact_addresses(&self) -> Result<Vec<(String, String, String)>> {
        let mut statement = self.db.prepare_cached(
            "SELECT lower(json_extract(e.value, '$.address')), c.account, c.id
             FROM contacts c, json_each(c.emails_json) e",
        )?;
        let rows = statement.query_map([], |row| Ok((row.get::<_, Option<String>>(0)?.unwrap_or_default(), row.get(1)?, row.get(2)?)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// The contact with this address, in any account: account and id.
    pub fn contact_with_address(&self, address: &str) -> Result<Option<(String, String)>> {
        let mut statement = self.db.prepare_cached(
            "SELECT c.account, c.id FROM contacts c
             WHERE EXISTS (
                 SELECT 1 FROM json_each(c.emails_json) e
                 WHERE lower(json_extract(e.value, '$.address')) = lower(?1))
             LIMIT 1",
        )?;
        Ok(statement.query_row([address.trim()], |row| Ok((row.get(0)?, row.get(1)?))).optional()?)
    }

    /// Addresses for completing a recipient: contacts first, then the
    /// people mail was sent to or came from. Those written to count three
    /// times as much as senders, automatic senders (no-reply) not at all,
    /// and the most recent come first among equals. Each comes with the
    /// name it was last seen with.
    pub fn suggest_addresses(&self, prefix: &str, limit: usize) -> Result<Vec<Address>> {
        let prefix = prefix.trim();
        if prefix.is_empty() {
            return Ok(Vec::new());
        }
        let pattern = format!("{}%", prefix.replace(['%', '_'], ""));
        let word = format!("% {}%", prefix.replace(['%', '_'], ""));
        let mut seen = std::collections::HashSet::new();
        let mut found = Vec::new();
        for contact in self.contacts(prefix)? {
            for email in &contact.emails {
                if seen.insert(email.address.to_lowercase()) {
                    found.push(Address {
                        name: if email.name.is_empty() { contact.display_name.clone() } else { email.name.clone() },
                        address: email.address.clone(),
                    });
                }
            }
        }
        let mut statement = self.db.prepare_cached(
            "WITH sent AS (
                 SELECT m.to_json, m.cc_json, m.received FROM messages m
                 JOIN folders f ON f.account = m.account AND f.id = m.folder_id
                 WHERE f.well_known = 'sentitems'
             ),
             people (name, address, weight, at) AS (
                 SELECT from_name, from_address,
                        CASE WHEN from_address LIKE '%noreply%' OR from_address LIKE '%no_reply%'
                             OR from_address LIKE '%donotreply%' OR from_address LIKE '%do_not_reply%'
                             THEN 0 ELSE 1 END,
                        received
                 FROM messages WHERE from_address != ''
                 UNION ALL
                 SELECT json_extract(r.value, '$.name'), json_extract(r.value, '$.address'), 3, sent.received
                 FROM sent, json_each(sent.to_json) r
                 UNION ALL
                 SELECT json_extract(r.value, '$.name'), json_extract(r.value, '$.address'), 3, sent.received
                 FROM sent, json_each(sent.cc_json) r
             )
             SELECT max(CASE WHEN name != '' AND name != address THEN printf('%020d', at) || name END),
                    max(printf('%020d', at) || address)
             FROM people
             WHERE address != '' AND (address LIKE ?1 OR name LIKE ?1 OR name LIKE ?2)
             GROUP BY lower(address)
             ORDER BY SUM(weight) DESC, MAX(at) DESC
             LIMIT ?3",
        )?;
        let rows = statement.query_map(params![pattern, word, limit as i64], |row| {
            // Each follows the time it was seen, which picked the latest.
            let unstamp = |stamped: Option<String>| stamped.and_then(|s| s.get(20..).map(str::to_owned)).unwrap_or_default();
            Ok(Address { name: unstamp(row.get(0)?), address: unstamp(row.get(1)?) })
        })?;
        for address in rows {
            let address = address?;
            if seen.insert(address.address.to_lowercase()) {
                found.push(address);
            }
        }
        found.truncate(limit);
        Ok(found)
    }
}

fn event_from(row: &Row) -> rusqlite::Result<Event> {
    Ok(Event {
        account: row.get(0)?,
        id: row.get(1)?,
        calendar_id: row.get(2)?,
        subject: row.get(3)?,
        start: row.get(4)?,
        end: row.get(5)?,
        all_day: row.get(6)?,
        location: row.get(7)?,
        organizer: json_or_default(&row.get::<_, String>(8)?),
        attendees: json_or_default::<Vec<Attendee>>(&row.get::<_, String>(9)?),
        preview: row.get(10)?,
        body_html: row.get(11)?,
        response: Response::from_graph(&row.get::<_, String>(12)?),
        show_as: row.get(13)?,
        is_cancelled: row.get(14)?,
        is_online: row.get(15)?,
        join_url: row.get(16)?,
        is_recurring: row.get(17)?,
        web_link: row.get(18)?,
    })
}

fn contact_from(row: &Row) -> rusqlite::Result<Contact> {
    Ok(Contact {
        account: row.get(0)?,
        id: row.get(1)?,
        display_name: row.get(2)?,
        given_name: row.get(3)?,
        surname: row.get(4)?,
        emails: json_or_default(&row.get::<_, String>(5)?),
        phones: json_or_default::<Vec<Phone>>(&row.get::<_, String>(6)?),
        company: row.get(7)?,
        job_title: row.get(8)?,
        notes: row.get(9)?,
    })
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or(0)
}

/// An FTS5 query matching every word as a prefix, with the user's text
/// quoted so it can't be read as query syntax.
fn fts_query(query: &str) -> Option<String> {
    let terms: Vec<String> = query
        .split_whitespace()
        .map(|word| word.replace('"', ""))
        .filter(|word| !word.is_empty())
        .map(|word| format!("\"{word}\"*"))
        .collect();
    (!terms.is_empty()).then(|| terms.join(" "))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(id: &str, folder: &str, subject: &str, received: i64) -> MessageRecord {
        MessageRecord {
            summary: MessageSummary {
                account: "a".into(),
                id: id.into(),
                folder_id: folder.into(),
                conversation_id: String::new(),
                subject: subject.into(),
                from: Address { name: "Ana Pérez".into(), address: "ana@example.com".into() },
                to: vec![],
                preview: "preview text".into(),
                received,
                is_read: false,
                is_draft: false,
                flag: Flag::None,
                high_importance: false,
                has_attachments: false,
            },
            cc: vec![],
            reply_to: vec![],
            sent: received,
            categories: vec!["Blue".into()],
        }
    }

    fn folder(id: &str) -> Folder {
        Folder { id: id.into(), parent_id: None, name: id.into(), well_known: None, unread: 0, total: 0 }
    }

    #[test]
    fn folders_list_newest_first() {
        let mut store = Store::open_in_memory().unwrap();
        store
            .put_messages(&[record("1", "inbox", "older", 10), record("2", "inbox", "newer", 20), record("3", "other", "x", 30)])
            .unwrap();
        let rows = store.folder_rows("a", "inbox", false).unwrap();
        let subjects: Vec<_> = rows.iter().map(|row| store.summary(*row).unwrap().unwrap().subject).collect();
        assert_eq!(subjects, ["newer", "older"]);
    }

    #[test]
    fn updates_keep_the_body() {
        let mut store = Store::open_in_memory().unwrap();
        store.put_messages(&[record("1", "inbox", "hello", 10)]).unwrap();
        store.put_body("a", "1", "<p>body</p>", "body", &[]).unwrap();
        let mut changed = record("1", "inbox", "hello again", 10);
        changed.summary.is_read = true;
        store.put_messages(&[changed]).unwrap();
        let detail = store.message("a", "1").unwrap().unwrap();
        assert_eq!(detail.body_html.as_deref(), Some("<p>body</p>"));
        assert!(detail.summary.is_read);
        assert_eq!(detail.categories, ["Blue"]);
    }

    #[test]
    fn search_matches_prefixes_and_bodies_without_accents() {
        let mut store = Store::open_in_memory().unwrap();
        store.put_messages(&[record("1", "inbox", "Quarterly report", 10), record("2", "inbox", "Lunch", 20)]).unwrap();
        store.put_body("a", "2", "", "the invoice is attached", &[]).unwrap();
        assert_eq!(store.search_rows(None, "quart", 10).unwrap().len(), 1);
        assert_eq!(store.search_rows(Some("a"), "invoice", 10).unwrap().len(), 1);
        assert_eq!(store.search_rows(None, "perez", 10).unwrap().len(), 2);
        assert_eq!(store.search_rows(None, "\"unbalanced", 10).unwrap().len(), 0);
        store.remove_messages("a", &["2".into()]).unwrap();
        assert_eq!(store.search_rows(None, "invoice", 10).unwrap().len(), 0);
    }

    #[test]
    fn gone_folders_take_their_messages() {
        let mut store = Store::open_in_memory().unwrap();
        store.put_folders("a", &[folder("inbox"), folder("old")]).unwrap();
        store.put_messages(&[record("1", "inbox", "x", 1), record("2", "old", "y", 2)]).unwrap();
        store.set_sync_link("a", "messages:old", Some("link")).unwrap();
        store.put_folders("a", &[folder("inbox")]).unwrap();
        assert!(store.row_of("a", "2").unwrap().is_none());
        assert!(store.row_of("a", "1").unwrap().is_some());
        assert!(store.sync_link("a", "messages:old").unwrap().is_none());
    }

    #[test]
    fn older_caches_gain_the_new_tables() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cache.sqlite3");
        {
            let db = Connection::open(&path).unwrap();
            db.execute_batch(SCHEMA).unwrap();
            db.pragma_update(None, "user_version", 1).unwrap();
        }
        let mut store = Store::open(&path).unwrap();
        let contact = Contact {
            account: "a".into(),
            id: "c".into(),
            display_name: "Ana".into(),
            emails: vec![Address { name: "Ana".into(), address: "Ana@Example.com".into() }],
            ..Contact::default()
        };
        store.put_contacts(&[contact]).unwrap();
        assert_eq!(store.contacts_without_photos("a", 10, 5).unwrap(), ["c"]);
        store.put_photo("a", "c", Some(&[1, 2, 3]), 20).unwrap();
        assert!(store.contacts_without_photos("a", 10, 5).unwrap().is_empty());
        assert_eq!(store.contact_photo("a", "c").unwrap(), Some(vec![1, 2, 3]));
        assert_eq!(store.contact_with_address("ANA@example.com").unwrap(), Some(("a".into(), "c".into())));
        assert_eq!(store.contact_with_address("bo@example.com").unwrap(), None);
        assert_eq!(store.contact_addresses().unwrap(), [("ana@example.com".into(), "a".into(), "c".into())]);
        drop(store);
        let store = Store::open(&path).unwrap();
        let version: i64 = store.db.pragma_query_value(None, "user_version", |row| row.get(0)).unwrap();
        assert_eq!(version, SCHEMA_VERSION);
        store.clear_contacts("a").unwrap();
        store.drop_orphan_photos().unwrap();
        assert_eq!(store.contact_photo("a", "c").unwrap(), None);
    }

    #[test]
    fn messages_with_a_person_come_newest_first() {
        let mut store = Store::open_in_memory().unwrap();
        let mut to_ana = record("2", "sent", "to", 5);
        to_ana.summary.from = Address { name: "Me".into(), address: "me@example.com".into() };
        to_ana.summary.to = vec![Address { name: "Ana".into(), address: "ANA@example.com".into() }];
        let mut to_anabel = record("3", "sent", "other", 6);
        to_anabel.summary.from = to_ana.summary.from.clone();
        to_anabel.summary.to = vec![Address { name: "Anabel".into(), address: "xana@example.com".into() }];
        store.put_messages(&[record("1", "inbox", "from", 1), to_ana, to_anabel]).unwrap();
        let found = store.messages_with(&["ana@example.com".into()], 10).unwrap();
        let subjects: Vec<&str> = found.iter().map(|m| m.subject.as_str()).collect();
        assert_eq!(subjects, ["to", "from"]);
    }

    #[test]
    #[ignore = "timing, run by hand"]
    fn suggestions_stay_fast() {
        let mut store = Store::open_in_memory().unwrap();
        let mut sent = folder("sent");
        sent.well_known = Some("sentitems".into());
        store.put_folders("a", &[folder("inbox"), sent]).unwrap();
        let records: Vec<MessageRecord> = (0..50_000)
            .map(|i| {
                let mut message = record(&i.to_string(), if i % 10 == 0 { "sent" } else { "inbox" }, "subject", i);
                message.summary.from = Address { name: format!("Person {}", i % 3000), address: format!("p{}@example.com", i % 3000) };
                message.summary.to = vec![Address { name: format!("Andrea {}", i % 500), address: format!("andrea{}@example.com", i % 500) }];
                message.cc = vec![Address { name: "Bo".into(), address: "bo@example.com".into() }];
                message
            })
            .collect();
        store.put_messages(&records).unwrap();
        for typed in ["an", "andrea 4", "p12"] {
            let start = std::time::Instant::now();
            let found = store.suggest_addresses(typed, 6).unwrap();
            eprintln!("{typed}: {} in {:?}", found.len(), start.elapsed());
        }
    }

    #[test]
    fn suggestions_come_from_senders() {
        let mut store = Store::open_in_memory().unwrap();
        store.put_messages(&[record("1", "inbox", "x", 1), record("2", "inbox", "y", 2)]).unwrap();
        let found = store.suggest_addresses("pér", 5).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].address, "ana@example.com");
    }

    #[test]
    fn suggestions_rank_people_written_to_first() {
        let mut store = Store::open_in_memory().unwrap();
        let mut sent_folder = folder("sent");
        sent_folder.well_known = Some("sentitems".into());
        store.put_folders("a", &[folder("inbox"), sent_folder]).unwrap();
        let from = |id: &str, name: &str, address: &str, received: i64| {
            let mut message = record(id, "inbox", "x", received);
            message.summary.from = Address { name: name.into(), address: address.into() };
            message
        };
        let mut written = record("3", "sent", "y", 5);
        written.summary.from = Address { name: "Me".into(), address: "me@example.com".into() };
        written.summary.to = vec![Address { name: "Anabel Old".into(), address: "anabel@example.com".into() }];
        let mut again = record("4", "sent", "z", 9);
        again.summary.from = written.summary.from.clone();
        again.cc = vec![Address { name: "Anabel Ruiz".into(), address: "Anabel@example.com".into() }];
        store
            .put_messages(&[
                from("1", "Ana Pérez", "ana@example.com", 1),
                from("2", "Ana Pérez", "ana@example.com", 2),
                from("5", "Anagram", "noreply@anagram.test", 8),
                written,
                again,
            ])
            .unwrap();
        let found = store.suggest_addresses("ana", 5).unwrap();
        let addresses: Vec<&str> = found.iter().map(|a| a.address.as_str()).collect();
        assert_eq!(addresses, ["Anabel@example.com", "ana@example.com", "noreply@anagram.test"]);
        // The name it was last seen with.
        assert_eq!(found[0].name, "Anabel Ruiz");
        // People written to are found by their names too.
        assert_eq!(store.suggest_addresses("ruiz", 5).unwrap()[0].address, "Anabel@example.com");
    }
}
