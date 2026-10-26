//! Bringing the cache up to date with the server.
//!
//! Folders are listed whole: there are few, and the list carries the unread
//! counts. Messages use delta queries per folder, resumable page by page.
//! Calendars and contacts are re-read for a window and replaced.

use std::collections::HashMap;
use std::sync::Mutex;

use serde::Deserialize;

use crate::graph::{Page, encode};
use crate::model::{Address, Attendee, Calendar, Contact, Event, Flag, Folder, MessageSummary, Phone, Response, wire};
use crate::store::MessageRecord;
use crate::{Graph, Result, Store, when};

/// The folders every mailbox has, in the order Outlook lists them.
pub const WELL_KNOWN: &[&str] = &["inbox", "drafts", "sentitems", "archive", "deleteditems", "junkemail", "outbox"];
// Messages shown at once when a folder is opened for the first time, before
// the delta query fills in the rest.
const FIRST_PAGE: u32 = 50;
const FOLDER_FIELDS: &str = "id,displayName,parentFolderId,childFolderCount,unreadItemCount,totalItemCount";
const EVENT_FIELDS: &str = "id,subject,start,end,isAllDay,location,organizer,attendees,bodyPreview,\
    responseStatus,showAs,isCancelled,isOnlineMeeting,onlineMeeting,type,seriesMasterId,webLink";
const CONTACT_FIELDS: &str = "id,displayName,givenName,surname,emailAddresses,mobilePhone,\
    businessPhones,homePhones,companyName,jobTitle,personalNotes";

fn lock(store: &Mutex<Store>) -> std::sync::MutexGuard<'_, Store> {
    store.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

pub fn folder_key(folder: &str) -> String {
    format!("messages:{folder}")
}

// Folders

#[derive(Deserialize)]
struct BatchResponse {
    responses: Vec<BatchItem>,
}

#[derive(Deserialize)]
struct BatchItem {
    id: String,
    status: u16,
    #[serde(default)]
    body: serde_json::Value,
}

/// The ids of the well-known folders, looked up once in a single batch.
async fn well_known_ids(graph: &Graph, store: &Mutex<Store>) -> Result<HashMap<String, String>> {
    let account = graph.account_id();
    let mut ids = HashMap::new();
    {
        let store = lock(store);
        for name in WELL_KNOWN {
            if let Some(id) = store.sync_link(account, &format!("wellknown:{name}"))? {
                ids.insert(id, (*name).to_owned());
            }
        }
    }
    if !ids.is_empty() {
        return Ok(ids);
    }
    let requests: Vec<_> = WELL_KNOWN
        .iter()
        .map(|name| {
            // Headers on the batch itself don't reach the requests inside it.
            serde_json::json!({
                "id": name,
                "method": "GET",
                "url": format!("/me/mailFolders/{name}?$select=id"),
                "headers": { "Prefer": "IdType=\"ImmutableId\"" },
            })
        })
        .collect();
    let response: BatchResponse = graph.post("/$batch", &serde_json::json!({ "requests": requests })).await?;
    let store = lock(store);
    for item in response.responses {
        if item.status == 200
            && let Some(id) = item.body.get("id").and_then(|id| id.as_str())
        {
            store.set_sync_link(account, &format!("wellknown:{}", item.id), Some(id))?;
            ids.insert(id.to_owned(), item.id);
        }
    }
    Ok(ids)
}

/// Re-reads the folder tree and its counts. Returns the folders, in the
/// order they are shown: well-known first, the rest by name, children
/// under their parents.
pub async fn sync_folders(graph: &Graph, store: &Mutex<Store>) -> Result<Vec<Folder>> {
    let well_known = well_known_ids(graph, store).await?;
    let mut all: Vec<wire::MailFolder> =
        graph.all(&format!("/me/mailFolders?$top=100&$select={FOLDER_FIELDS}")).await?;
    let mut pending: Vec<String> = all.iter().filter(|f| f.child_folder_count > 0).map(|f| f.id.clone()).collect();
    while let Some(parent) = pending.pop() {
        let children: Vec<wire::MailFolder> = graph
            .all(&format!("/me/mailFolders/{}/childFolders?$top=100&$select={FOLDER_FIELDS}", encode(&parent)))
            .await?;
        pending.extend(children.iter().filter(|f| f.child_folder_count > 0).map(|f| f.id.clone()));
        all.extend(children);
    }
    let folders = order_folders(all, &well_known);
    lock(store).put_folders(graph.account_id(), &folders)?;
    Ok(folders)
}

fn order_folders(all: Vec<wire::MailFolder>, well_known: &HashMap<String, String>) -> Vec<Folder> {
    let ids: std::collections::HashSet<&str> = all.iter().map(|f| f.id.as_str()).collect();
    let mut children: HashMap<Option<String>, Vec<Folder>> = HashMap::new();
    for folder in &all {
        // The top-level folders' parent is the hidden root.
        let parent = folder.parent_folder_id.clone().filter(|parent| ids.contains(parent.as_str()));
        children.entry(parent.clone()).or_default().push(Folder {
            id: folder.id.clone(),
            parent_id: parent,
            name: folder.display_name.clone(),
            well_known: well_known.get(&folder.id).cloned(),
            unread: folder.unread_item_count,
            total: folder.total_item_count,
        });
    }
    let rank = |folder: &Folder| {
        folder
            .well_known
            .as_deref()
            .and_then(|name| WELL_KNOWN.iter().position(|known| *known == name))
            .unwrap_or(WELL_KNOWN.len())
    };
    for list in children.values_mut() {
        list.sort_by(|a, b| rank(a).cmp(&rank(b)).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
    }
    let mut ordered = Vec::with_capacity(all.len());
    fn visit(parent: Option<String>, children: &mut HashMap<Option<String>, Vec<Folder>>, ordered: &mut Vec<Folder>) {
        for folder in children.remove(&parent).unwrap_or_default() {
            let id = folder.id.clone();
            ordered.push(folder);
            visit(Some(id), children, ordered);
        }
    }
    visit(None, &mut children, &mut ordered);
    ordered
}

// Messages

pub fn record_from(account: &str, message: &wire::Message, folder: &str) -> MessageRecord {
    let addresses = |list: &Option<Vec<wire::Recipient>>| -> Vec<Address> {
        list.as_deref().unwrap_or_default().iter().map(Address::from).collect()
    };
    MessageRecord {
        summary: MessageSummary {
            account: account.to_owned(),
            id: message.id.clone(),
            folder_id: message.parent_folder_id.clone().unwrap_or_else(|| folder.to_owned()),
            conversation_id: message.conversation_id.clone().unwrap_or_default(),
            subject: message.subject.clone().unwrap_or_default(),
            from: message.from.as_ref().map(Address::from).unwrap_or_default(),
            to: addresses(&message.to_recipients),
            preview: message.body_preview.clone().unwrap_or_default(),
            received: message.received_date_time.as_deref().map(when::parse_instant).unwrap_or(0),
            is_read: message.is_read.unwrap_or(true),
            is_draft: message.is_draft.unwrap_or(false),
            flag: message.flag.as_ref().map(|flag| Flag::from_graph(&flag.flag_status)).unwrap_or_default(),
            high_importance: message.importance.as_deref() == Some("high"),
            has_attachments: message.has_attachments.unwrap_or(false),
        },
        cc: addresses(&message.cc_recipients),
        reply_to: addresses(&message.reply_to),
        sent: message.sent_date_time.as_deref().map(when::parse_instant).unwrap_or(0),
        categories: message.categories.clone().unwrap_or_default(),
    }
}

#[derive(Debug, Default)]
pub struct FolderSync {
    /// Anything in the folder changed.
    pub changed: bool,
    /// Unread messages that arrived since the last sync, for notifications.
    pub arrived: Vec<MessageSummary>,
}

/// Applies one page of a delta query or listing.
fn apply_messages(
    store: &Mutex<Store>,
    account: &str,
    folder: &str,
    page: Vec<wire::Message>,
    initial: bool,
    result: &mut FolderSync,
) -> Result<()> {
    let mut store = lock(store);
    let mut removed = Vec::new();
    let mut records = Vec::new();
    for message in page {
        if message.removed.is_some() {
            // A message that moved here from another watched folder was
            // already brought in there; only drop it if it is still ours.
            if store.folder_of(account, &message.id)?.is_none_or(|current| current == folder) {
                removed.push(message.id);
            }
            continue;
        }
        // Delta pages may carry only the fields that changed: apply those
        // to the cached message rather than blanking the rest.
        if message.received_date_time.is_none() && message.subject.is_none() {
            if store.row_of(account, &message.id)?.is_some() {
                if let Some(read) = message.is_read {
                    store.set_read(account, &message.id, read)?;
                    result.changed = true;
                }
                if let Some(flag) = &message.flag {
                    store.set_flag(account, &message.id, Flag::from_graph(&flag.flag_status))?;
                    result.changed = true;
                }
                if let Some(folder) = &message.parent_folder_id {
                    store.set_folder(account, &message.id, folder)?;
                    result.changed = true;
                }
            }
            continue;
        }
        let record = record_from(account, &message, folder);
        if !initial && !record.summary.is_read && store.row_of(account, &record.summary.id)?.is_none() {
            result.arrived.push(record.summary.clone());
        }
        records.push(record);
    }
    if !removed.is_empty() || !records.is_empty() {
        result.changed = true;
    }
    store.remove_messages(account, &removed)?;
    store.put_messages(&records)?;
    Ok(())
}

/// Brings one folder's messages up to date. `on_page` is called after each
/// page is stored, so the list can fill in during a long first sync.
pub async fn sync_messages(
    graph: &Graph,
    store: &Mutex<Store>,
    folder: &str,
    mut on_page: impl FnMut(),
) -> Result<FolderSync> {
    let account = graph.account_id().to_owned();
    let key = folder_key(folder);
    let mut result = FolderSync::default();
    let saved = lock(store).sync_link(&account, &key)?;
    let initial = saved.is_none();
    if initial {
        // The newest messages first, so the folder isn't empty while the
        // delta query works through the rest.
        let first: Page<wire::Message> = graph
            .page(&format!(
                "/me/mailFolders/{}/messages?$top={FIRST_PAGE}&$orderby=receivedDateTime%20desc&$select={}",
                encode(folder),
                wire::MESSAGE_FIELDS
            ))
            .await?;
        apply_messages(store, &account, folder, first.value, true, &mut result)?;
        on_page();
    }
    let mut link = saved.unwrap_or_else(|| {
        format!("/me/mailFolders/{}/messages/delta?$select={}", encode(folder), wire::MESSAGE_FIELDS)
    });
    loop {
        let page: Page<wire::Message> = match graph.page(&link).await {
            Ok(page) => page,
            Err(error) if error.is_sync_reset() || (error.is_not_found() && !initial) => {
                log::info!("starting {folder} over: {error}");
                let store = lock(store);
                store.set_sync_link(&account, &key, None)?;
                return Err(error);
            }
            Err(error) => return Err(error),
        };
        // Only a page that changed something refreshes the list: an Inbox
        // checked every few seconds would otherwise be redrawn each time,
        // losing the pointer's place and the selection's anchor.
        let changed_before = std::mem::take(&mut result.changed);
        apply_messages(store, &account, folder, page.value, initial, &mut result)?;
        if result.changed {
            on_page();
        }
        result.changed |= changed_before;
        match (page.next_link, page.delta_link) {
            (Some(next), _) => {
                lock(store).set_sync_link(&account, &key, Some(&next))?;
                link = next;
            }
            (None, Some(delta)) => {
                lock(store).set_sync_link(&account, &key, Some(&delta))?;
                break;
            }
            (None, None) => break,
        }
    }
    Ok(result)
}

// Calendars

pub fn event_from(account: &str, calendar: &str, event: &wire::Event) -> Event {
    let all_day = event.is_all_day.unwrap_or(false);
    let time = |value: &Option<wire::DateTimeTimeZone>| {
        let instant = value.as_ref().map(|value| when::parse_utc(&value.date_time)).unwrap_or(0);
        // Graph gives the midnights of an all-day event in UTC; the date is
        // the one that midnight falls on where the user is.
        if all_day { when::date_seconds(when::local(instant).date_naive()) } else { instant }
    };
    Event {
        account: account.to_owned(),
        id: event.id.clone(),
        calendar_id: event.calendar.as_ref().map(|c| c.id.clone()).unwrap_or_else(|| calendar.to_owned()),
        subject: event.subject.clone().unwrap_or_default(),
        start: time(&event.start),
        end: time(&event.end),
        all_day,
        location: event.location.as_ref().and_then(|l| l.display_name.clone()).unwrap_or_default(),
        organizer: event.organizer.as_ref().map(Address::from).unwrap_or_default(),
        attendees: event
            .attendees
            .as_deref()
            .unwrap_or_default()
            .iter()
            .map(|attendee| Attendee {
                address: Address {
                    name: attendee.email_address.name.clone().unwrap_or_default(),
                    address: attendee.email_address.address.clone().unwrap_or_default(),
                },
                response: attendee.status.as_ref().map(|s| Response::from_graph(&s.response)).unwrap_or_default(),
                kind: attendee.kind.clone().unwrap_or_default(),
            })
            .collect(),
        preview: event.body_preview.clone().unwrap_or_default(),
        body_html: event.body.as_ref().map(|body| body.content.clone()).unwrap_or_default(),
        response: event.response_status.as_ref().map(|s| Response::from_graph(&s.response)).unwrap_or_default(),
        show_as: event.show_as.clone().unwrap_or_else(|| "busy".into()),
        is_cancelled: event.is_cancelled.unwrap_or(false),
        is_online: event.is_online_meeting.unwrap_or(false),
        join_url: event.online_meeting.as_ref().and_then(|m| m.join_url.clone()).unwrap_or_default(),
        is_recurring: event.series_master_id.is_some() || event.kind.as_deref() == Some("occurrence"),
        web_link: event.web_link.clone().unwrap_or_default(),
    }
}

#[derive(serde::Deserialize)]
struct TimeZoneSetting {
    value: String,
}

/// The mailbox's time zone, as Outlook names it, kept for creating events.
pub async fn sync_time_zone(graph: &Graph, store: &Mutex<Store>) -> Result<()> {
    let zone: TimeZoneSetting = graph.get("/me/mailboxSettings/timeZone").await?;
    lock(store).set_sync_link(graph.account_id(), TIME_ZONE_KEY, Some(&zone.value))?;
    Ok(())
}

pub const TIME_ZONE_KEY: &str = "timezone";

pub async fn sync_calendars(graph: &Graph, store: &Mutex<Store>) -> Result<Vec<Calendar>> {
    let calendars: Vec<wire::Calendar> =
        graph.all("/me/calendars?$top=100&$select=id,name,hexColor,canEdit,isDefaultCalendar").await?;
    let calendars: Vec<Calendar> = calendars
        .into_iter()
        .map(|calendar| Calendar {
            id: calendar.id,
            name: calendar.name,
            color: calendar.hex_color.filter(|color| color.starts_with('#')).unwrap_or_default(),
            can_edit: calendar.can_edit,
            is_default: calendar.is_default_calendar,
        })
        .collect();
    lock(store).put_calendars(graph.account_id(), &calendars)?;
    Ok(calendars)
}

/// Replaces the cached events of `calendar` that overlap `[start, end)`.
pub async fn sync_events(graph: &Graph, store: &Mutex<Store>, calendar: &str, start: i64, end: i64) -> Result<()> {
    let account = graph.account_id();
    let path = format!(
        "/me/calendars/{}/calendarView?startDateTime={}Z&endDateTime={}Z&$top=100&$select={EVENT_FIELDS}",
        encode(calendar),
        when::format_utc(start),
        when::format_utc(end)
    );
    let events: Vec<wire::Event> = graph.all(&path).await?;
    let events: Vec<Event> = events.iter().map(|event| event_from(account, calendar, event)).collect();
    let mut store = lock(store);
    let stale = store.event_ids_in(account, calendar, start, end)?;
    store.remove_events(account, &stale)?;
    store.put_events(&events)?;
    Ok(())
}

// Contacts

pub fn contact_from(account: &str, contact: &wire::Contact) -> Contact {
    let mut phones = Vec::new();
    if let Some(mobile) = contact.mobile_phone.as_ref().filter(|p| !p.is_empty()) {
        phones.push(Phone { kind: "mobile".into(), number: mobile.clone() });
    }
    for (kind, list) in [("business", &contact.business_phones), ("home", &contact.home_phones)] {
        for number in list.as_deref().unwrap_or_default() {
            if !number.is_empty() {
                phones.push(Phone { kind: kind.into(), number: number.clone() });
            }
        }
    }
    Contact {
        account: account.to_owned(),
        id: contact.id.clone(),
        display_name: contact.display_name.clone().unwrap_or_default(),
        given_name: contact.given_name.clone().unwrap_or_default(),
        surname: contact.surname.clone().unwrap_or_default(),
        emails: contact
            .email_addresses
            .as_deref()
            .unwrap_or_default()
            .iter()
            .filter_map(|email| {
                Some(Address { name: email.name.clone().unwrap_or_default(), address: email.address.clone()? })
            })
            .collect(),
        phones,
        company: contact.company_name.clone().unwrap_or_default(),
        job_title: contact.job_title.clone().unwrap_or_default(),
        notes: contact.personal_notes.clone().unwrap_or_default(),
    }
}

pub async fn sync_contacts(graph: &Graph, store: &Mutex<Store>) -> Result<()> {
    let account = graph.account_id();
    let contacts: Vec<wire::Contact> = graph.all(&format!("/me/contacts?$top=100&$select={CONTACT_FIELDS}")).await?;
    let contacts: Vec<Contact> = contacts.iter().map(|contact| contact_from(account, contact)).collect();
    let mut store = lock(store);
    store.clear_contacts(account)?;
    store.put_contacts(&contacts)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn folder(id: &str, name: &str, parent: Option<&str>) -> wire::MailFolder {
        wire::MailFolder {
            id: id.into(),
            display_name: name.into(),
            parent_folder_id: Some(parent.unwrap_or("root").into()),
            child_folder_count: 0,
            unread_item_count: 0,
            total_item_count: 0,
        }
    }

    #[test]
    fn folders_order_well_known_first_and_children_under_parents() {
        let all = vec![
            folder("z", "Zeta", None),
            folder("sent", "Sent Items", None),
            folder("a", "alpha", None),
            folder("in", "Inbox", None),
            folder("child", "Receipts", Some("in")),
        ];
        let well_known: HashMap<_, _> =
            [("in".to_owned(), "inbox".to_owned()), ("sent".to_owned(), "sentitems".to_owned())].into();
        let names: Vec<_> = order_folders(all, &well_known).into_iter().map(|f| (f.name, f.parent_id)).collect();
        assert_eq!(names, [
            ("Inbox".to_owned(), None),
            ("Receipts".to_owned(), Some("in".to_owned())),
            ("Sent Items".to_owned(), None),
            ("alpha".to_owned(), None),
            ("Zeta".to_owned(), None),
        ]);
    }

    #[test]
    fn delta_messages_parse() {
        let json = r#"{"value":[
            {"id":"1","subject":"Hi","receivedDateTime":"2024-01-02T03:04:05Z","isRead":false,
             "from":{"emailAddress":{"name":"Ana","address":"ana@x.test"}},
             "flag":{"flagStatus":"flagged"},"importance":"high","parentFolderId":"inbox"},
            {"id":"2","@removed":{"reason":"deleted"}}
        ],"@odata.deltaLink":"https://graph.microsoft.com/v1.0/delta?token=x"}"#;
        let page: Page<wire::Message> = serde_json::from_str(json).unwrap();
        assert!(page.delta_link.is_some());
        let record = record_from("a", &page.value[0], "inbox");
        assert_eq!(record.summary.from.display(), "Ana");
        assert_eq!(record.summary.flag, Flag::Flagged);
        assert!(record.summary.high_importance && !record.summary.is_read);
        assert!(page.value[1].removed.is_some());
    }

    #[test]
    fn a_removal_spares_a_message_that_moved_to_another_folder() {
        let store = Mutex::new(Store::open_in_memory().unwrap());
        let message: wire::Message = serde_json::from_str(
            r#"{"id":"m","subject":"s","receivedDateTime":"2026-10-01T00:00:00Z","parentFolderId":"inbox"}"#,
        )
        .unwrap();
        store.lock().unwrap().put_messages(&[record_from("a", &message, "inbox")]).unwrap();
        let removed: wire::Message = serde_json::from_str(r#"{"id":"m","@removed":{"reason":"deleted"}}"#).unwrap();
        let mut result = FolderSync::default();
        apply_messages(&store, "a", "custom", vec![removed], false, &mut result).unwrap();
        assert!(store.lock().unwrap().row_of("a", "m").unwrap().is_some(), "it lives in the inbox now");
        let removed: wire::Message = serde_json::from_str(r#"{"id":"m","@removed":{"reason":"deleted"}}"#).unwrap();
        apply_messages(&store, "a", "inbox", vec![removed], false, &mut result).unwrap();
        assert!(store.lock().unwrap().row_of("a", "m").unwrap().is_none());
    }

    #[test]
    fn all_day_events_keep_their_local_date() {
        // Graph sends the local midnight in UTC: the day before, east of UTC.
        let midnight = when::local_midnight(chrono::NaiveDate::from_ymd_opt(2026, 10, 20).unwrap());
        let json = format!(
            r#"{{"id":"e","isAllDay":true,"start":{{"dateTime":"{}","timeZone":"UTC"}},"end":{{"dateTime":"{}","timeZone":"UTC"}}}}"#,
            when::format_utc(midnight),
            when::format_utc(midnight + 86400)
        );
        let event = event_from("a", "c", &serde_json::from_str(&json).unwrap());
        assert_eq!(when::floating_date(event.start), chrono::NaiveDate::from_ymd_opt(2026, 10, 20).unwrap());
        assert_eq!(event.end - event.start, 86400);
    }

    #[test]
    fn events_parse_in_utc() {
        let json = r#"{"id":"e","subject":"Standup","isAllDay":false,
            "start":{"dateTime":"1970-01-01T00:01:00.0000000","timeZone":"UTC"},
            "end":{"dateTime":"1970-01-01T00:02:00.0000000","timeZone":"UTC"},
            "responseStatus":{"response":"tentativelyAccepted"},
            "attendees":[{"emailAddress":{"name":"Bo","address":"bo@x.test"},"status":{"response":"accepted"},"type":"required"}],
            "onlineMeeting":{"joinUrl":"https://teams.test/j"},"isOnlineMeeting":true}"#;
        let event: wire::Event = serde_json::from_str(json).unwrap();
        let event = event_from("a", "cal", &event);
        assert_eq!((event.start, event.end), (60, 120));
        assert_eq!(event.response, Response::Tentative);
        assert_eq!(event.attendees[0].response, Response::Accepted);
        assert_eq!(event.join_url, "https://teams.test/j");
    }
}
