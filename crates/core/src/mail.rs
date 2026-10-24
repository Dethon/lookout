//! Reading and changing messages, and sending new ones.

use std::sync::Mutex;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use serde::Deserialize;
use serde_json::json;

use crate::graph::{Page, encode};
use crate::model::{Address, Attachment, Flag, MessageDetail, MessageSummary, wire};
use crate::{Graph, Result, Store, html};

// Attachments up to this size go in one request; larger ones are uploaded
// in chunks, as Graph limits a request to 4 MB.
const INLINE_ATTACHMENT_LIMIT: usize = 3 * 1024 * 1024;

fn lock(store: &Mutex<Store>) -> std::sync::MutexGuard<'_, Store> {
    store.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Takes a message out of its folder's counts and adds it to those of
/// `destination`, or to none if it is going for good.
fn recount(store: &Store, summary: &MessageSummary, destination: Option<&str>) -> Result<()> {
    if destination == Some(summary.folder_id.as_str()) {
        return Ok(());
    }
    let unread = i64::from(!summary.is_read);
    store.adjust_counts(&summary.account, &summary.folder_id, -unread, -1)?;
    if let Some(destination) = destination {
        store.adjust_counts(&summary.account, destination, unread, 1)?;
    }
    Ok(())
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct BodyOnly {
    body: Option<wire::ItemBody>,
    bcc_recipients: Option<Vec<wire::Recipient>>,
}

/// The message with its body, fetching the body if it isn't cached.
pub async fn open(graph: &Graph, store: &Mutex<Store>, id: &str) -> Result<Option<MessageDetail>> {
    let account = graph.account_id();
    let cached = lock(store).message(account, id)?;
    match cached {
        Some(detail) if detail.body_html.is_some() => {
            lock(store).touch_body(account, id)?;
            Ok(Some(detail))
        }
        Some(mut detail) => {
            let path = format!("/me/messages/{}?$select=body,bccRecipients", encode(id));
            let fetched: BodyOnly = graph.get(&path).await?;
            let body = fetched.body.unwrap_or_default();
            let html = if body.content_type.eq_ignore_ascii_case("text") {
                html::text_to_html(&body.content)
            } else {
                body.content
            };
            let attachments =
                if detail.summary.has_attachments || html.contains("cid:") { attachments(graph, id).await? } else { Vec::new() };
            let text = html::render(&html).text;
            let store = lock(store);
            store.put_body(account, id, &html, &text, &attachments)?;
            store.trim_bodies()?;
            detail.body_html = Some(html);
            detail.attachments = attachments;
            detail.bcc = fetched.bcc_recipients.as_deref().unwrap_or_default().iter().map(Address::from).collect();
            Ok(Some(detail))
        }
        None => Ok(None),
    }
}

async fn attachments(graph: &Graph, id: &str) -> Result<Vec<Attachment>> {
    let base = format!("/me/messages/{}/attachments?$select=id,name,contentType,size,isInline", encode(id));
    // The content id of inline pictures is only on file attachments.
    let with_content_id = format!("{base},microsoft.graph.fileAttachment/contentId");
    let list: Vec<wire::AttachmentInfo> = match graph.all(&with_content_id).await {
        Ok(list) => list,
        Err(crate::Error::Graph { status: 400, .. }) => graph.all(&base).await?,
        Err(error) => return Err(error),
    };
    Ok(list
        .into_iter()
        .map(|info| Attachment {
            id: info.id,
            name: info.name.unwrap_or_else(|| "attachment".into()),
            content_type: info.content_type.unwrap_or_default(),
            size: info.size,
            is_inline: info.is_inline,
            content_id: info.content_id,
        })
        .collect())
}

/// An attachment's bytes.
pub async fn attachment_bytes(graph: &Graph, message: &str, attachment: &str) -> Result<Vec<u8>> {
    graph
        .bytes(&format!("/me/messages/{}/attachments/{}/$value", encode(message), encode(attachment)))
        .await
}

/// Marks a message read or unread: in the cache at once, then on the
/// server. `cached` is called in between, so the list can show the change.
pub async fn set_read(graph: &Graph, store: &Mutex<Store>, id: &str, read: bool, cached: impl FnOnce()) -> Result<()> {
    let account = graph.account_id();
    {
        let store = lock(store);
        if let Some(detail) = store.message(account, id)?
            && detail.summary.is_read != read
        {
            store.set_read(account, id, read)?;
            store.adjust_counts(account, &detail.summary.folder_id, if read { -1 } else { 1 }, 0)?;
        }
    }
    cached();
    graph.patch_empty(&format!("/me/messages/{}", encode(id)), &json!({ "isRead": read })).await
}

pub async fn set_flag(graph: &Graph, store: &Mutex<Store>, id: &str, flag: Flag, cached: impl FnOnce()) -> Result<()> {
    lock(store).set_flag(graph.account_id(), id, flag)?;
    cached();
    graph
        .patch_empty(&format!("/me/messages/{}", encode(id)), &json!({ "flag": { "flagStatus": flag.to_graph() } }))
        .await
}

#[derive(Deserialize)]
struct Moved {
    id: String,
}

/// Moves a message to another folder; `destination` may be a well-known
/// name such as `archive`.
pub async fn move_to(graph: &Graph, store: &Mutex<Store>, id: &str, destination: &str, cached: impl FnOnce()) -> Result<()> {
    let account = graph.account_id();
    let folder = lock(store).well_known_folder(account, destination)?.unwrap_or_else(|| destination.to_owned());
    let unread = {
        let store = lock(store);
        let detail = store.message(account, id)?;
        if let Some(detail) = &detail {
            recount(&store, &detail.summary, Some(&folder))?;
            store.set_folder(account, id, &folder)?;
        }
        detail
    };
    cached();
    let moved: Moved =
        graph.post(&format!("/me/messages/{}/move", encode(id)), &json!({ "destinationId": folder })).await?;
    // Immutable ids survive a move; if the service still issued a new one,
    // the next sync of the destination brings it in.
    if moved.id != id && unread.is_some() {
        lock(store).remove_messages(account, &[id.to_owned()])?;
    }
    Ok(())
}

/// Moves a message to Deleted Items, or deletes it for good if it is there.
pub async fn delete(graph: &Graph, store: &Mutex<Store>, id: &str, cached: impl FnOnce()) -> Result<()> {
    let account = graph.account_id();
    let (summary, deleted_items) = {
        let store = lock(store);
        (store.message(account, id)?.map(|detail| detail.summary), store.well_known_folder(account, "deleteditems")?)
    };
    if let Some(summary) = summary.filter(|summary| Some(&summary.folder_id) == deleted_items.as_ref()) {
        {
            let mut store = lock(store);
            recount(&store, &summary, None)?;
            store.remove_messages(account, &[id.to_owned()])?;
        }
        cached();
        graph.delete(&format!("/me/messages/{}", encode(id))).await
    } else {
        move_to(graph, store, id, "deleteditems", cached).await
    }
}

/// A change made to several messages at once.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Bulk {
    Read(bool),
    Flag(Flag),
    /// To a folder id or a well-known name such as `archive`.
    Move(String),
    /// To Deleted Items, or for good for those already there.
    Delete,
}

// Graph takes at most this many requests in one batch.
const BATCH: usize = 20;

#[derive(Deserialize)]
struct BatchReply {
    responses: Vec<BatchItem>,
}

#[derive(Deserialize)]
struct BatchItem {
    status: u16,
    #[serde(default)]
    body: serde_json::Value,
}

/// Applies `change` to the messages `ids`: in the cache at once (`cached`
/// is called after), then on the server, twenty to a request.
pub async fn bulk(graph: &Graph, store: &Mutex<Store>, ids: &[String], change: &Bulk, cached: impl FnOnce()) -> Result<()> {
    let account = graph.account_id();
    let mut requests = Vec::with_capacity(ids.len());
    {
        let mut store = lock(store);
        let folder = match change {
            Bulk::Move(destination) => {
                Some(store.well_known_folder(account, destination)?.unwrap_or_else(|| destination.clone()))
            }
            _ => None,
        };
        let deleted_items = store.well_known_folder(account, "deleteditems")?;
        let mut forget = Vec::new();
        for id in ids {
            let Some(detail) = store.message(account, id)? else { continue };
            let summary = &detail.summary;
            let url = format!("/me/messages/{}", encode(id));
            let request = match change {
                Bulk::Read(read) => {
                    if summary.is_read != *read {
                        store.set_read(account, id, *read)?;
                        store.adjust_counts(account, &summary.folder_id, if *read { -1 } else { 1 }, 0)?;
                    }
                    json!({ "method": "PATCH", "url": url, "body": { "isRead": read } })
                }
                Bulk::Flag(flag) => {
                    store.set_flag(account, id, *flag)?;
                    json!({ "method": "PATCH", "url": url, "body": { "flag": { "flagStatus": flag.to_graph() } } })
                }
                Bulk::Delete if Some(&summary.folder_id) == deleted_items.as_ref() => {
                    recount(&store, summary, None)?;
                    forget.push(id.clone());
                    json!({ "method": "DELETE", "url": url })
                }
                Bulk::Move(_) | Bulk::Delete => {
                    let destination = folder.clone().or_else(|| deleted_items.clone()).unwrap_or_else(|| "deleteditems".into());
                    recount(&store, summary, Some(&destination))?;
                    store.set_folder(account, id, &destination)?;
                    json!({ "method": "POST", "url": format!("{url}/move"), "body": { "destinationId": destination } })
                }
            };
            requests.push(request);
        }
        store.remove_messages(account, &forget)?;
    }
    cached();
    let mut failed = 0;
    let mut first_error = None;
    for chunk in requests.chunks(BATCH) {
        let numbered: Vec<_> = chunk
            .iter()
            .enumerate()
            .map(|(index, request)| {
                let mut request = request.clone();
                request["id"] = json!(index.to_string());
                request["headers"] = json!({ "Content-Type": "application/json", "Prefer": "IdType=\"ImmutableId\"" });
                request
            })
            .collect();
        let reply: BatchReply = graph.post("/$batch", &json!({ "requests": numbered })).await?;
        for item in reply.responses.iter().filter(|item| item.status >= 300) {
            failed += 1;
            first_error.get_or_insert_with(|| {
                item.body["error"]["message"].as_str().unwrap_or("refused").to_owned()
            });
        }
    }
    match first_error {
        Some(message) => Err(crate::Error::Graph {
            status: 207,
            code: "batch".into(),
            message: format!("{failed} of {} not changed: {message}", requests.len()),
        }),
        None => Ok(()),
    }
}

/// A file to attach.
#[derive(Debug, Clone)]
pub struct NewAttachment {
    pub name: String,
    pub content_type: String,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplyKind {
    Reply,
    ReplyAll,
    Forward,
}

/// A message being written.
#[derive(Debug, Clone, Default)]
pub struct Draft {
    pub to: Vec<Address>,
    pub cc: Vec<Address>,
    pub bcc: Vec<Address>,
    pub subject: String,
    /// The text the user wrote, as HTML; for replies it goes above the quote.
    pub body_html: String,
    pub attachments: Vec<NewAttachment>,
    pub high_importance: bool,
    /// The message replied to or forwarded.
    pub reply: Option<(ReplyKind, String)>,
}

/// Prepares the server-side draft a reply starts from: recipients and
/// subject filled in, the original quoted.
pub async fn start_reply(graph: &Graph, kind: ReplyKind, id: &str) -> Result<wire::Message> {
    let action = match kind {
        ReplyKind::Reply => "createReply",
        ReplyKind::ReplyAll => "createReplyAll",
        ReplyKind::Forward => "createForward",
    };
    graph.post(&format!("/me/messages/{}/{action}", encode(id)), &json!({})).await
}

fn recipients(list: &[Address]) -> Vec<wire::Recipient> {
    list.iter().map(wire::Recipient::from).collect()
}

/// Puts `ours` at the top of a reply draft's body, above the quote.
fn insert_above_quote(draft_html: &str, ours: &str) -> String {
    let lower = draft_html.to_ascii_lowercase();
    if let Some(body) = lower.find("<body")
        && let Some(end) = lower[body..].find('>')
    {
        let at = body + end + 1;
        return format!("{}{ours}{}", &draft_html[..at], &draft_html[at..]);
    }
    format!("{ours}{draft_html}")
}

/// Sends a message. Replies and forwards keep the thread and the quoted
/// original; `reply_draft` is the draft from [`start_reply`] if one was made.
pub async fn send(graph: &Graph, draft: &Draft, reply_draft: Option<&wire::Message>) -> Result<()> {
    let mut fields = json!({
        "subject": draft.subject,
        "toRecipients": recipients(&draft.to),
        "ccRecipients": recipients(&draft.cc),
        "bccRecipients": recipients(&draft.bcc),
        "importance": if draft.high_importance { "high" } else { "normal" },
    });

    let id = match (&draft.reply, reply_draft) {
        (Some(_), Some(existing)) => {
            let quoted = existing.body.as_ref().map(|body| body.content.as_str()).unwrap_or_default();
            fields["body"] = json!({ "contentType": "html", "content": insert_above_quote(quoted, &draft.body_html) });
            graph.patch_empty(&format!("/me/messages/{}", encode(&existing.id)), &fields).await?;
            existing.id.clone()
        }
        (Some((kind, original)), None) => {
            let existing = start_reply(graph, *kind, original).await?;
            return Box::pin(send(graph, draft, Some(&existing))).await;
        }
        (None, _) => {
            fields["body"] = json!({ "contentType": "html", "content": draft.body_html });
            let created: Moved = graph.post("/me/messages", &fields).await?;
            created.id
        }
    };

    for attachment in &draft.attachments {
        add_attachment(graph, &id, attachment).await?;
    }
    graph.post_empty::<()>(&format!("/me/messages/{}/send", encode(&id)), None).await
}

/// Sends a draft saved earlier, with the fields as now edited.
pub async fn send_draft(graph: &Graph, id: &str, draft: &Draft) -> Result<()> {
    let fields = json!({
        "subject": draft.subject,
        "toRecipients": recipients(&draft.to),
        "ccRecipients": recipients(&draft.cc),
        "bccRecipients": recipients(&draft.bcc),
        "importance": if draft.high_importance { "high" } else { "normal" },
        "body": { "contentType": "html", "content": draft.body_html },
    });
    graph.patch_empty(&format!("/me/messages/{}", encode(id)), &fields).await?;
    for attachment in &draft.attachments {
        add_attachment(graph, id, attachment).await?;
    }
    graph.post_empty::<()>(&format!("/me/messages/{}/send", encode(id)), None).await
}

/// Saves a message as a draft without sending it.
pub async fn save_draft(graph: &Graph, draft: &Draft) -> Result<String> {
    let fields = json!({
        "subject": draft.subject,
        "toRecipients": recipients(&draft.to),
        "ccRecipients": recipients(&draft.cc),
        "bccRecipients": recipients(&draft.bcc),
        "body": { "contentType": "html", "content": draft.body_html },
    });
    let created: Moved = graph.post("/me/messages", &fields).await?;
    for attachment in &draft.attachments {
        add_attachment(graph, &created.id, attachment).await?;
    }
    Ok(created.id)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct UploadSession {
    upload_url: String,
}

async fn add_attachment(graph: &Graph, message: &str, attachment: &NewAttachment) -> Result<()> {
    let content_type =
        if attachment.content_type.is_empty() { "application/octet-stream" } else { &attachment.content_type };
    if attachment.bytes.len() <= INLINE_ATTACHMENT_LIMIT {
        let body = json!({
            "@odata.type": "#microsoft.graph.fileAttachment",
            "name": attachment.name,
            "contentType": content_type,
            "contentBytes": STANDARD.encode(&attachment.bytes),
        });
        let _: serde_json::Value = graph.post(&format!("/me/messages/{}/attachments", encode(message)), &body).await?;
        return Ok(());
    }
    let session: UploadSession = graph
        .post(
            &format!("/me/messages/{}/attachments/createUploadSession", encode(message)),
            &json!({ "AttachmentItem": {
                "attachmentType": "file",
                "name": attachment.name,
                "size": attachment.bytes.len(),
                "contentType": content_type,
            }}),
        )
        .await?;
    graph.upload(&session.upload_url, &attachment.bytes).await
}

/// Searches the whole mailbox on the server, for mail older than the cache.
pub async fn search_server(graph: &Graph, store: &Mutex<Store>, query: &str) -> Result<Vec<String>> {
    let quoted = format!("\"{}\"", query.replace('"', ""));
    let path = format!("/me/messages?$search={}&$top=50&$select={}", encode(&quoted), wire::MESSAGE_FIELDS);
    let page: Page<wire::Message> = graph.page(&path).await?;
    let account = graph.account_id();
    let records: Vec<_> = page
        .value
        .iter()
        .filter(|message| message.removed.is_none())
        .map(|message| crate::sync::record_from(account, message, ""))
        .collect();
    let ids = records.iter().map(|record| record.summary.id.clone()).collect();
    lock(store).put_messages(&records)?;
    Ok(ids)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replies_go_above_the_quote() {
        let draft = "<html><head></head><BODY dir=\"ltr\"><div id=\"divRplyFwdMsg\">original</div></body></html>";
        assert_eq!(
            insert_above_quote(draft, "<p>mine</p>"),
            "<html><head></head><BODY dir=\"ltr\"><p>mine</p><div id=\"divRplyFwdMsg\">original</div></body></html>"
        );
        assert_eq!(insert_above_quote("<div>q</div>", "<p>m</p>"), "<p>m</p><div>q</div>");
    }
}
