//! Creating, changing and answering events.

use std::sync::Mutex;

use serde_json::json;

use crate::graph::encode;
use crate::model::{Address, Event, wire};
use crate::{Graph, Result, Store, sync, when};

fn lock(store: &Mutex<Store>) -> std::sync::MutexGuard<'_, Store> {
    store.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// An event being created or edited.
#[derive(Debug, Clone, Default)]
pub struct EventDraft {
    pub calendar_id: String,
    pub subject: String,
    /// UTC seconds; for all-day events, the local date's midnight in UTC.
    pub start: i64,
    pub end: i64,
    pub all_day: bool,
    pub location: String,
    pub body_html: String,
    pub attendees: Vec<Address>,
    /// Adds a Teams meeting (work and school accounts).
    pub online: bool,
    /// `free`, `tentative`, `busy`, `oof`.
    pub show_as: String,
    /// Minutes before the start; `None` for no reminder.
    pub reminder: Option<u32>,
    /// The zone all-day events are dated in: the mailbox's, else this
    /// computer's.
    pub time_zone: String,
}

fn fields(draft: &EventDraft) -> serde_json::Value {
    let time = |seconds: i64| {
        if draft.all_day {
            // All-day events are dates in the user's zone, not instants.
            json!({
                "dateTime": format!("{}T00:00:00", when::floating_date(seconds).format("%Y-%m-%d")),
                "timeZone": if draft.time_zone.is_empty() { when::zone_name() } else { draft.time_zone.clone() },
            })
        } else {
            json!({ "dateTime": when::format_utc(seconds), "timeZone": "UTC" })
        }
    };
    let mut value = json!({
        "subject": draft.subject,
        "start": time(draft.start),
        "end": time(draft.end),
        "isAllDay": draft.all_day,
        "location": { "displayName": draft.location },
        "body": { "contentType": "html", "content": draft.body_html },
        "attendees": draft.attendees.iter().map(|address| json!({
            "emailAddress": { "address": address.address, "name": address.name },
            "type": "required",
        })).collect::<Vec<_>>(),
        "showAs": if draft.show_as.is_empty() { "busy" } else { &draft.show_as },
        "isReminderOn": draft.reminder.is_some(),
    });
    if let Some(minutes) = draft.reminder {
        value["reminderMinutesBeforeStart"] = json!(minutes);
    }
    if draft.online {
        value["isOnlineMeeting"] = json!(true);
        value["onlineMeetingProvider"] = json!("teamsForBusiness");
    }
    value
}

pub async fn create(graph: &Graph, store: &Mutex<Store>, draft: &EventDraft) -> Result<Event> {
    let path = if draft.calendar_id.is_empty() {
        "/me/events".to_owned()
    } else {
        format!("/me/calendars/{}/events", encode(&draft.calendar_id))
    };
    let created: wire::Event = graph.post(&path, &fields(draft)).await?;
    let event = sync::event_from(graph.account_id(), &draft.calendar_id, &created);
    lock(store).put_events(std::slice::from_ref(&event))?;
    Ok(event)
}

pub async fn update(graph: &Graph, store: &Mutex<Store>, id: &str, draft: &EventDraft) -> Result<Event> {
    let mut body = fields(draft);
    // Turning a meeting online can't be undone through Graph.
    if !draft.online {
        body.as_object_mut().expect("an object").remove("isOnlineMeeting");
    }
    let updated: wire::Event = graph.patch(&format!("/me/events/{}", encode(id)), &body).await?;
    let event = sync::event_from(graph.account_id(), &draft.calendar_id, &updated);
    lock(store).put_events(std::slice::from_ref(&event))?;
    Ok(event)
}

/// Deletes an event; for a meeting the user organises, `cancel_message`
/// cancels it for the attendees with that note.
pub async fn delete(graph: &Graph, store: &Mutex<Store>, id: &str, cancel_message: Option<&str>) -> Result<()> {
    lock(store).remove_events(graph.account_id(), &[id.to_owned()])?;
    match cancel_message {
        Some(comment) => {
            graph.post_empty(&format!("/me/events/{}/cancel", encode(id)), Some(&json!({ "comment": comment }))).await
        }
        None => graph.delete(&format!("/me/events/{}", encode(id))).await,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Answer {
    Accept,
    Tentative,
    Decline,
}

pub async fn respond(graph: &Graph, store: &Mutex<Store>, id: &str, answer: Answer, comment: &str, notify: bool) -> Result<()> {
    let action = match answer {
        Answer::Accept => "accept",
        Answer::Tentative => "tentativelyAccept",
        Answer::Decline => "decline",
    };
    graph
        .post_empty(&format!("/me/events/{}/{action}", encode(id)), Some(&json!({ "comment": comment, "sendResponse": notify })))
        .await?;
    let account = graph.account_id();
    let mut store = lock(store);
    if answer == Answer::Decline {
        store.remove_events(account, &[id.to_owned()])?;
    } else if let Some(mut event) = store.event(account, id)? {
        event.response = if answer == Answer::Accept {
            crate::model::Response::Accepted
        } else {
            crate::model::Response::Tentative
        };
        store.put_events(&[event])?;
    }
    Ok(())
}

#[derive(serde::Deserialize)]
struct BodyOnly {
    body: Option<wire::ItemBody>,
}

/// The event's description, fetched when it is opened.
pub async fn body(graph: &Graph, store: &Mutex<Store>, id: &str) -> Result<String> {
    let fetched: BodyOnly = graph.get(&format!("/me/events/{}?$select=body", encode(id))).await?;
    let body = fetched.body.unwrap_or_default();
    let html = if body.content_type.eq_ignore_ascii_case("text") {
        crate::html::text_to_html(&body.content)
    } else {
        body.content
    };
    let account = graph.account_id();
    let mut store = lock(store);
    if let Some(mut event) = store.event(account, id)? {
        event.body_html = html.clone();
        store.put_events(&[event])?;
    }
    Ok(html)
}
