//! The message list as a lazy model: it holds only the row ids of the
//! folder and reads the rows on screen from the cache, so a folder of fifty
//! thousand messages costs a few hundred kilobytes.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use lookout_core::Store;
use lookout_core::model::{Flag, MessageSummary};
use lookout_core::store::RowId;
use slint::{Model, ModelNotify, ModelTracker};

use crate::MessageRow;
use crate::photos::Photos;

// Rows kept converted; a screenful is a few dozen.
const CACHE_ROWS: usize = 400;

pub struct MessageList {
    store: Rc<Store>,
    photos: Rc<Photos>,
    rows: RefCell<Vec<RowId>>,
    cache: RefCell<HashMap<RowId, (MessageSummary, MessageRow)>>,
    notify: ModelNotify,
    now: Cell<chrono::DateTime<chrono::Local>>,
    /// The rows picked with Ctrl or Shift, besides the open one.
    selected: RefCell<HashSet<RowId>>,
    /// Where a Shift range starts.
    anchor: Cell<Option<RowId>>,
}

impl MessageList {
    pub fn new(store: Rc<Store>, photos: Rc<Photos>) -> Rc<Self> {
        Rc::new(Self {
            store,
            photos,
            rows: RefCell::new(Vec::new()),
            cache: RefCell::new(HashMap::new()),
            notify: ModelNotify::default(),
            now: Cell::new(chrono::Local::now()),
            selected: RefCell::new(HashSet::new()),
            anchor: Cell::new(None),
        })
    }

    /// Shows these rows instead, keeping the selection of those that stay.
    pub fn set_rows(&self, rows: Vec<RowId>) {
        {
            let kept: HashSet<RowId> = rows.iter().copied().collect();
            self.selected.borrow_mut().retain(|row| kept.contains(row));
        }
        *self.rows.borrow_mut() = rows;
        self.cache.borrow_mut().clear();
        self.now.set(chrono::Local::now());
        self.notify.reset();
    }

    /// Selects only the row at `index`.
    pub fn select_one(&self, index: usize) {
        let Some(row) = self.rows.borrow().get(index).copied() else { return };
        let before: Vec<RowId> = self.selected.borrow_mut().drain().collect();
        self.selected.borrow_mut().insert(row);
        self.anchor.set(Some(row));
        self.changed(before.into_iter().chain([row]));
    }

    /// Adds or removes the row at `index`, as Ctrl+click does.
    pub fn toggle(&self, index: usize) {
        let Some(row) = self.rows.borrow().get(index).copied() else { return };
        {
            let mut selected = self.selected.borrow_mut();
            if !selected.remove(&row) {
                selected.insert(row);
            }
        }
        self.anchor.set(Some(row));
        self.changed([row]);
    }

    /// Selects from the anchor to `index`, as Shift+click does.
    pub fn select_range(&self, index: usize) {
        let anchor = self.anchor.get().and_then(|row| self.rows.borrow().iter().position(|r| *r == row)).unwrap_or(index);
        let (from, to) = (anchor.min(index), anchor.max(index));
        let range: Vec<RowId> = self.rows.borrow().get(from..=to.min(self.len().saturating_sub(1))).unwrap_or_default().to_vec();
        let before: Vec<RowId> = self.selected.borrow_mut().drain().collect();
        self.selected.borrow_mut().extend(range.iter().copied());
        self.changed(before.into_iter().chain(range));
    }

    pub fn clear(&self) {
        let before: Vec<RowId> = self.selected.borrow_mut().drain().collect();
        self.changed(before);
    }

    pub fn select_all(&self) {
        let all: Vec<RowId> = self.rows.borrow().clone();
        self.selected.borrow_mut().extend(all.iter().copied());
        self.notify.reset();
    }

    /// The selected messages, in list order.
    pub fn selection(&self) -> Vec<MessageSummary> {
        let selected = self.selected.borrow();
        let rows: Vec<RowId> = self.rows.borrow().iter().filter(|row| selected.contains(row)).copied().collect();
        drop(selected);
        rows.into_iter().filter_map(|row| self.load(row).map(|(summary, _)| summary)).collect()
    }

    pub fn selection_len(&self) -> usize {
        self.selected.borrow().len()
    }

    pub fn is_selected(&self, index: usize) -> bool {
        self.rows.borrow().get(index).is_some_and(|row| self.selected.borrow().contains(row))
    }

    /// Redraws the rows whose selection changed.
    fn changed(&self, rows: impl IntoIterator<Item = RowId>) {
        let rows: HashSet<RowId> = rows.into_iter().collect();
        if rows.len() > 64 {
            self.notify.reset();
            return;
        }
        for (index, row) in self.rows.borrow().iter().enumerate() {
            if rows.contains(row) {
                self.notify.row_changed(index);
            }
        }
    }

    pub fn len(&self) -> usize {
        self.rows.borrow().len()
    }

    /// Reads the rows again, as when contacts' pictures were found.
    pub fn refresh(&self) {
        self.cache.borrow_mut().clear();
        self.notify.reset();
    }

    pub fn summary(&self, index: usize) -> Option<MessageSummary> {
        let row = *self.rows.borrow().get(index)?;
        self.load(row).map(|(summary, _)| summary)
    }

    pub fn index_of(&self, account: &str, id: &str) -> Option<usize> {
        let row = self.store.row_of(account, id).ok().flatten()?;
        self.rows.borrow().iter().position(|candidate| *candidate == row)
    }

    fn load(&self, row: RowId) -> Option<(MessageSummary, MessageRow)> {
        if let Some(hit) = self.cache.borrow().get(&row) {
            return Some(hit.clone());
        }
        let summary = self.store.summary(row).ok().flatten()?;
        let mut display = to_row(&summary, self.now.get());
        display.junk = in_junk(&self.store, &summary);
        display.picture = self.photos.for_address(&self.store, &person_of(&summary).address);
        let mut cache = self.cache.borrow_mut();
        if cache.len() >= CACHE_ROWS {
            cache.clear();
        }
        cache.insert(row, (summary.clone(), display.clone()));
        Some((summary, display))
    }
}

/// The stretch of time a message falls in, as the list heads it.
pub fn group_of(received: i64, now: chrono::DateTime<chrono::Local>) -> String {
    use chrono::Datelike;
    let date = lookout_core::when::local(received).date_naive();
    let today = now.date_naive();
    let days = (today - date).num_days();
    let monday = today - chrono::Days::new(u64::from(today.weekday().num_days_from_monday()));
    if days <= 0 {
        "Today".into()
    } else if days == 1 {
        "Yesterday".into()
    } else if date >= monday {
        date.format("%A").to_string()
    } else if date >= monday - chrono::Days::new(7) {
        "Last week".into()
    } else if date.year() == today.year() && date.month() == today.month() {
        "Earlier this month".into()
    } else if date.year() == today.year() {
        date.format("%B").to_string()
    } else {
        date.year().to_string()
    }
}

/// Whether the message is in its account's Junk Email folder.
pub fn in_junk(store: &Store, summary: &MessageSummary) -> bool {
    store.well_known_folder(&summary.account, "junkemail").ok().flatten().is_some_and(|junk| junk == summary.folder_id)
}

/// Who a row shows: the sender, or in Sent Items and Drafts the first
/// recipient.
fn person_of(summary: &MessageSummary) -> lookout_core::model::Address {
    if summary.is_draft || summary.from.address.is_empty() {
        summary.to.first().cloned().unwrap_or_default()
    } else {
        summary.from.clone()
    }
}

pub fn to_row(summary: &MessageSummary, now: chrono::DateTime<chrono::Local>) -> MessageRow {
    let person = person_of(summary);
    // In Sent Items and Drafts the other side is who it went to.
    let sender = if summary.is_draft || summary.from.address.is_empty() {
        crate::format::names(&summary.to)
    } else {
        summary.from.display().to_owned()
    };
    MessageRow {
        account: summary.account.as_str().into(),
        id: summary.id.as_str().into(),
        sender: if sender.is_empty() { "(no recipients)".into() } else { sender.into() },
        subject: summary.subject.as_str().into(),
        preview: summary.preview.split_whitespace().collect::<Vec<_>>().join(" ").into(),
        date: lookout_core::when::list_date(summary.received, now).into(),
        unread: !summary.is_read,
        flagged: summary.flag == Flag::Flagged,
        completed: summary.flag == Flag::Complete,
        attachment: summary.has_attachments,
        important: summary.high_importance,
        draft: summary.is_draft,
        initials: crate::format::initials(person.display()).into(),
        hue: crate::format::hue(&person.address),
        ..MessageRow::default()
    }
}

impl Model for MessageList {
    type Data = MessageRow;

    fn row_count(&self) -> usize {
        self.rows.borrow().len()
    }

    fn row_data(&self, index: usize) -> Option<MessageRow> {
        let row = *self.rows.borrow().get(index)?;
        let (summary, mut display) = self.load(row)?;
        let now = self.now.get();
        let group = group_of(summary.received, now);
        let previous = index.checked_sub(1).and_then(|i| self.rows.borrow().get(i).copied());
        let starts = match previous.and_then(|previous| self.load(previous)) {
            Some((before, _)) => group_of(before.received, now) != group,
            None => true,
        };
        display.group = if starts { group.into() } else { Default::default() };
        display.selected = self.selected.borrow().contains(&row);
        Some(display)
    }

    fn model_tracker(&self) -> &dyn ModelTracker {
        &self.notify
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn messages_group_by_how_long_ago() {
        // A Saturday.
        let now = chrono::Local.with_ymd_and_hms(2026, 10, 24, 18, 0, 0).unwrap();
        let at = |y, m, d| chrono::Local.with_ymd_and_hms(y, m, d, 12, 0, 0).unwrap().timestamp();
        assert_eq!(group_of(at(2026, 10, 24), now), "Today");
        assert_eq!(group_of(at(2026, 10, 23), now), "Yesterday");
        assert_eq!(group_of(at(2026, 10, 20), now), "Tuesday");
        assert_eq!(group_of(at(2026, 10, 14), now), "Last week");
        assert_eq!(group_of(at(2026, 10, 3), now), "Earlier this month");
        assert_eq!(group_of(at(2026, 8, 3), now), "August");
        assert_eq!(group_of(at(2025, 12, 31), now), "2025");
    }
}
