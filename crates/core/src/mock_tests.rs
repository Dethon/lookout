//! Syncing against a stand-in for Graph on a local port: the requests made,
//! paging, delta links and what ends up in the cache.

use std::sync::{Arc, Mutex};

use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use crate::model::Flag;
use crate::{Graph, Session, Store, mail, sync};

type Handler = Arc<dyn Fn(&str, &str, &str, &str) -> (u16, String) + Send + Sync>;

/// Serves requests with `handler(method, target, body, base)` and records
/// each `METHOD target`.
async fn serve(handler: Handler) -> (String, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let base = format!("http://127.0.0.1:{}/v1.0", listener.local_addr().unwrap().port());
    let log = Arc::new(Mutex::new(Vec::new()));
    let (served_base, served_log) = (base.clone(), log.clone());
    tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else { return };
            let handler = handler.clone();
            let (base, log) = (served_base.clone(), served_log.clone());
            tokio::spawn(async move {
                let mut data = Vec::new();
                let mut buffer = [0u8; 8192];
                let (head_end, length) = loop {
                    let n = stream.read(&mut buffer).await.unwrap_or(0);
                    if n == 0 {
                        return;
                    }
                    data.extend_from_slice(&buffer[..n]);
                    if let Some(end) = data.windows(4).position(|w| w == b"\r\n\r\n") {
                        let head = String::from_utf8_lossy(&data[..end]).to_ascii_lowercase();
                        let length = head
                            .lines()
                            .find_map(|line| line.strip_prefix("content-length:"))
                            .and_then(|value| value.trim().parse::<usize>().ok())
                            .unwrap_or(0);
                        break (end + 4, length);
                    }
                };
                while data.len() < head_end + length {
                    let n = stream.read(&mut buffer).await.unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    data.extend_from_slice(&buffer[..n]);
                }
                let head = String::from_utf8_lossy(&data[..head_end]).into_owned();
                let body = String::from_utf8_lossy(&data[head_end..]).into_owned();
                let mut request_line = head.lines().next().unwrap_or_default().split_whitespace();
                let method = request_line.next().unwrap_or_default().to_owned();
                let target = request_line.next().unwrap_or_default().to_owned();
                assert!(head.contains("Bearer token"), "unauthorised request: {head}");
                log.lock().unwrap().push(format!("{method} {target}"));
                let (status, reply) = handler(&method, &target, &body, &base);
                let response = format!(
                    "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}",
                    reply.len()
                );
                _ = stream.write_all(response.as_bytes()).await;
            });
        }
    });
    (base, log)
}

fn graph(base: &str) -> Graph {
    let http = reqwest::Client::new();
    Graph::with_base(http.clone(), Arc::new(Session::fixed(http, "acct", "token")), base)
}

fn message(id: &str, subject: &str, received: &str, read: bool) -> serde_json::Value {
    json!({
        "id": id, "subject": subject, "receivedDateTime": received, "isRead": read,
        "parentFolderId": "in", "bodyPreview": format!("about {subject}"),
        "from": { "emailAddress": { "name": "Ana", "address": "ana@x.test" } },
        "toRecipients": [{ "emailAddress": { "name": "Me", "address": "me@x.test" } }],
        "flag": { "flagStatus": "notFlagged" }, "hasAttachments": id == "m2",
    })
}

fn mailbox(method: &str, target: &str, body: &str, base: &str) -> (u16, String) {
    let path = target.strip_prefix("/v1.0").unwrap_or(target);
    let reply = match (method, path) {
        // Reports as not junk are taken; as junk, refused.
        ("POST", "/beta/$batch") => {
            let requests: serde_json::Value = serde_json::from_str(body).unwrap();
            let responses: Vec<_> = requests["requests"]
                .as_array()
                .unwrap()
                .iter()
                .map(|request| {
                    assert!(request["url"].as_str().unwrap().ends_with("/reportMessage"));
                    assert_eq!(request["body"]["IsMessageMoveRequested"], false);
                    let status = if request["body"]["ReportAction"] == "notJunk" { 200 } else { 400 };
                    json!({ "id": request["id"], "status": status, "body": {} })
                })
                .collect();
            json!({ "responses": responses })
        }
        ("POST", "/$batch") if body.contains("\"PATCH\"") || body.contains("/move") => {
            let requests: serde_json::Value = serde_json::from_str(body).unwrap();
            let responses: Vec<_> = requests["requests"]
                .as_array()
                .unwrap()
                .iter()
                .map(|request| {
                    assert!(request["headers"]["Prefer"].as_str().unwrap().contains("ImmutableId"));
                    json!({ "id": request["id"], "status": 200, "body": {} })
                })
                .collect();
            json!({ "responses": responses })
        }
        ("POST", "/$batch") => {
            let requests: serde_json::Value = serde_json::from_str(body).unwrap();
            let responses: Vec<_> = requests["requests"]
                .as_array()
                .unwrap()
                .iter()
                .map(|request| {
                    let id = request["id"].as_str().unwrap();
                    match id {
                        "inbox" => json!({ "id": id, "status": 200, "body": { "id": "in" } }),
                        "drafts" => json!({ "id": id, "status": 200, "body": { "id": "dr" } }),
                        _ => json!({ "id": id, "status": 404, "body": {} }),
                    }
                })
                .collect();
            json!({ "responses": responses })
        }
        ("GET", p) if p.starts_with("/me/mailFolders?$top=100") => json!({
            "value": [
                { "id": "dr", "displayName": "Drafts", "parentFolderId": "root", "totalItemCount": 1 },
                { "id": "in", "displayName": "Inbox", "parentFolderId": "root", "childFolderCount": 1, "unreadItemCount": 2, "totalItemCount": 3 },
            ],
            "@odata.nextLink": format!("{base}/me/mailFolders/page2"),
        }),
        ("GET", "/me/mailFolders/page2") => json!({
            "value": [{ "id": "zz", "displayName": "Zebra", "parentFolderId": "root" }],
        }),
        ("GET", p) if p.starts_with("/me/mailFolders/in/childFolders") => json!({
            "value": [{ "id": "kid", "displayName": "Receipts", "parentFolderId": "in" }],
        }),
        ("GET", p) if p.starts_with("/me/mailFolders/in/messages?$top=50&$orderby=receivedDateTime%20desc") => json!({
            "value": [message("m1", "first", "2026-10-01T10:00:00Z", true)],
        }),
        ("GET", p) if p.starts_with("/me/mailFolders/in/messages/delta?$select=") => json!({
            "value": [
                message("m1", "first", "2026-10-01T10:00:00Z", true),
                message("m2", "second", "2026-10-02T10:00:00Z", false),
            ],
            "@odata.nextLink": format!("{base}/delta-page-2"),
        }),
        ("GET", "/delta-page-2") | ("GET", "/v1.0/delta-page-2") => json!({
            "value": [
                message("m3", "third", "2026-10-03T10:00:00Z", false),
                { "id": "m1", "@removed": { "reason": "deleted" } },
            ],
            "@odata.deltaLink": format!("{base}/delta-token-1"),
        }),
        ("GET", "/delta-token-1") => json!({
            "value": [
                message("m4", "fourth", "2026-10-04T10:00:00Z", false),
                { "id": "m3", "isRead": true },
            ],
            "@odata.deltaLink": format!("{base}/delta-token-2"),
        }),
        ("GET", "/me/messages/m2?$select=body,bccRecipients") => json!({
            "body": { "contentType": "html", "content": "<p>Hello <b>there</b> <img src=\"cid:logo@x\"></p>" },
            "bccRecipients": [{ "emailAddress": { "address": "hidden@x.test" } }],
        }),
        ("GET", p) if p.starts_with("/me/messages/m2/attachments?$select=") => json!({
            "value": [
                { "id": "a1", "name": "report.pdf", "contentType": "application/pdf", "size": 1234, "isInline": false },
                { "id": "a2", "name": "logo.png", "contentType": "image/png", "size": 99, "isInline": true, "contentId": "logo@x" },
            ],
        }),
        ("PATCH", "/me/messages/m2") => json!({}),
        ("GET", p) if p.starts_with("/me/calendars?") => json!({
            "value": [{ "id": "cal", "name": "Calendar", "hexColor": "#0f6cbd", "canEdit": true, "isDefaultCalendar": true }],
        }),
        ("GET", p) if p.starts_with("/me/calendars/cal/calendarView?startDateTime=") => json!({
            "value": [{
                "id": "e1", "subject": "Standup", "isAllDay": false,
                "start": { "dateTime": "2026-10-20T09:00:00.0000000", "timeZone": "UTC" },
                "end": { "dateTime": "2026-10-20T09:30:00.0000000", "timeZone": "UTC" },
                "responseStatus": { "response": "organizer" },
            }],
        }),
        ("GET", p) if p.starts_with("/me/contacts?") => json!({
            "value": [{
                "id": "c1", "displayName": "Bo Lindqvist",
                "emailAddresses": [{ "name": "Bo", "address": "bo@x.test" }],
                "mobilePhone": "+46 1", "businessPhones": ["+46 2"],
            }],
        }),
        _ => return (404, json!({ "error": { "code": "notFound", "message": format!("no route for {method} {path}") } }).to_string()),
    };
    (200, reply.to_string())
}

#[tokio::test]
async fn a_mailbox_syncs_end_to_end() {
    let (base, log) = serve(Arc::new(mailbox)).await;
    let graph = graph(&base);
    let store = Mutex::new(Store::open_in_memory().unwrap());

    let folders = sync::sync_folders(&graph, &store).await.unwrap();
    let names: Vec<_> = folders.iter().map(|f| (f.name.as_str(), f.well_known.as_deref())).collect();
    assert_eq!(names, [("Inbox", Some("inbox")), ("Receipts", None), ("Drafts", Some("drafts")), ("Zebra", None)]);

    // First sync: the newest page, then the delta query over two pages.
    let mut pages = 0;
    let first = sync::sync_messages(&graph, &store, "in", || pages += 1).await.unwrap();
    assert!(first.changed && first.arrived.is_empty(), "no notifications for the first sync");
    assert_eq!(pages, 3);
    {
        let store = store.lock().unwrap();
        let rows = store.folder_rows("acct", "in", false).unwrap();
        let subjects: Vec<_> = rows.iter().map(|r| store.summary(*r).unwrap().unwrap().subject).collect();
        assert_eq!(subjects, ["third", "second"], "m1 was removed by the second page");
        assert_eq!(store.sync_link("acct", "messages:in").unwrap().unwrap(), format!("{base}/delta-token-1"));
    }

    // Later syncs follow the delta link and report new unread mail; a
    // change carrying only some fields updates those alone.
    let later = sync::sync_messages(&graph, &store, "in", || {}).await.unwrap();
    assert_eq!(later.arrived.iter().map(|m| m.subject.as_str()).collect::<Vec<_>>(), ["fourth"]);
    let m3 = store.lock().unwrap().message("acct", "m3").unwrap().unwrap().summary;
    assert_eq!((m3.subject.as_str(), m3.is_read), ("third", true));

    // Opening fetches the body once, with attachments and the inline picture's id.
    let detail = mail::open(&graph, &store, "m2").await.unwrap().unwrap();
    assert!(detail.body_html.unwrap().contains("<b>there</b>"));
    assert_eq!(detail.bcc[0].address, "hidden@x.test");
    assert_eq!(detail.attachments.len(), 2);
    assert_eq!(detail.attachments[1].content_id.as_deref(), Some("logo@x"));
    mail::open(&graph, &store, "m2").await.unwrap();
    assert_eq!(store.lock().unwrap().search_rows(None, "there", 10).unwrap().len(), 1, "the body is searchable");

    // Marking read changes the cache and the server.
    mail::set_read(&graph, &store, "m2", true, || {}).await.unwrap();
    assert!(store.lock().unwrap().message("acct", "m2").unwrap().unwrap().summary.is_read);
    mail::set_flag(&graph, &store, "m2", Flag::Flagged, || {}).await.unwrap_or(());

    // Several at once go in one batch.
    mail::bulk(&graph, &store, &["m2".into(), "m3".into()], &mail::Bulk::Read(false), || {}).await.unwrap();
    assert!(!store.lock().unwrap().message("acct", "m3").unwrap().unwrap().summary.is_read);
    let counts = |folder: &str| {
        let folders = store.lock().unwrap().folders("acct").unwrap();
        folders.into_iter().find(|f| f.id == folder).map(|f| (f.unread, f.total)).unwrap()
    };
    let (inbox, drafts) = (counts("in"), counts("dr"));
    mail::bulk(&graph, &store, &["m2".into(), "m3".into()], &mail::Bulk::Move("drafts".into()), || {}).await.unwrap();
    assert_eq!(store.lock().unwrap().folder_of("acct", "m2").unwrap().as_deref(), Some("dr"));
    // The folders' counts follow at once, without waiting for a sync.
    assert_eq!(counts("in"), (inbox.0 - 2, inbox.1 - 2));
    assert_eq!(counts("dr"), (drafts.0 + 2, drafts.1 + 2));

    // Not spam goes back to the Inbox and is reported, through the beta.
    mail::report(&graph, &store, &["m2".into()], mail::Spam::NotJunk, || {}).await.unwrap();
    assert_eq!(store.lock().unwrap().folder_of("acct", "m2").unwrap().as_deref(), Some("in"));
    assert!(log.lock().unwrap().iter().any(|r| r == "POST /beta/$batch"));
    // A refused report still leaves the message moved.
    mail::report(&graph, &store, &["m2".into()], mail::Spam::Junk, || {}).await.unwrap();
    assert_eq!(store.lock().unwrap().folder_of("acct", "m2").unwrap().as_deref(), Some("junkemail"));

    // Calendars and contacts.
    sync::sync_calendars(&graph, &store).await.unwrap();
    let start = crate::when::parse_instant("2026-10-19T00:00:00Z");
    sync::sync_events(&graph, &store, "cal", start, start + 7 * 86400).await.unwrap();
    let events = store.lock().unwrap().events_between(start, start + 7 * 86400).unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].end - events[0].start, 1800);
    sync::sync_contacts(&graph, &store).await.unwrap();
    let contacts = store.lock().unwrap().contacts("").unwrap();
    assert_eq!(contacts[0].phones.len(), 2);

    let requests = log.lock().unwrap();
    let body_fetches = requests.iter().filter(|r| r.starts_with("GET /v1.0/me/messages/m2?")).count();
    assert_eq!(body_fetches, 1, "the second open came from the cache: {requests:#?}");
}

#[tokio::test]
async fn throttling_is_waited_out() {
    let calls = Arc::new(Mutex::new(0));
    let counter = calls.clone();
    let handler: Handler = Arc::new(move |_, _, _, _| {
        let mut calls = counter.lock().unwrap();
        *calls += 1;
        if *calls == 1 {
            (429, json!({ "error": { "code": "TooManyRequests", "message": "slow down" } }).to_string())
        } else {
            (200, json!({ "value": [] }).to_string())
        }
    });
    let (base, _) = serve(handler).await;
    tokio::time::pause();
    let result: crate::Result<crate::graph::Page<serde_json::Value>> = graph(&base).page("/me/anything").await;
    assert!(result.is_ok());
    assert_eq!(*calls.lock().unwrap(), 2);
}

#[tokio::test]
async fn errors_carry_graphs_message() {
    let handler: Handler =
        Arc::new(|_, _, _, _| (403, json!({ "error": { "code": "ErrorAccessDenied", "message": "Access is denied." } }).to_string()));
    let (base, _) = serve(handler).await;
    let error = graph(&base).get::<serde_json::Value>("/me").await.unwrap_err();
    assert!(matches!(error, crate::Error::Graph { status: 403, ref message, .. } if message == "Access is denied."));
}

#[tokio::test]
async fn blocked_senders_are_kept_in_one_rule() {
    // The rule as the server holds it.
    let rule: Arc<Mutex<Option<serde_json::Value>>> = Arc::new(Mutex::new(None));
    let held = rule.clone();
    let handler: Handler = Arc::new(move |method, target, body, _| {
        let path = target.strip_prefix("/v1.0").unwrap_or(target);
        let mut rule = held.lock().unwrap();
        let reply = match (method, path) {
            ("GET", "/me/mailFolders/inbox/messageRules") => {
                let rules: Vec<_> = [json!({ "id": "other", "displayName": "Newsletters" })].into_iter().chain(rule.clone()).collect();
                json!({ "value": rules })
            }
            ("GET", "/me/mailFolders/junkemail?$select=id") => json!({ "id": "jk" }),
            ("POST", "/me/mailFolders/inbox/messageRules") => {
                let mut created: serde_json::Value = serde_json::from_str(body).unwrap();
                created["id"] = json!("r1");
                *rule = Some(created.clone());
                return (201, created.to_string());
            }
            ("PATCH", "/me/mailFolders/inbox/messageRules/r1") => {
                let change: serde_json::Value = serde_json::from_str(body).unwrap();
                rule.as_mut().unwrap()["conditions"] = change["conditions"].clone();
                json!({})
            }
            ("DELETE", "/me/mailFolders/inbox/messageRules/r1") => {
                *rule = None;
                return (204, String::new());
            }
            _ => return (404, json!({ "error": { "code": "notFound", "message": path } }).to_string()),
        };
        (200, reply.to_string())
    });
    let (base, log) = serve(handler).await;
    let graph = graph(&base);
    let store = Mutex::new(Store::open_in_memory().unwrap());
    let addresses = |rule: &Option<serde_json::Value>| -> Vec<String> {
        rule.as_ref().map_or_else(Vec::new, |rule| {
            rule["conditions"]["fromAddresses"].as_array().unwrap().iter().map(|r| r["emailAddress"]["address"].as_str().unwrap().to_owned()).collect()
        })
    };
    let block = |list: &[&str], blocked| {
        let list: Vec<String> = list.iter().map(|s| s.to_string()).collect();
        let graph = graph.clone();
        let store = &store;
        async move { crate::rules::block_senders(&graph, store, &list, blocked, "me@x.test", || panic!("no browser in tests")).await }
    };

    // The first sender creates the rule, moving to Junk Email by its id.
    block(&["ads@x.test"], true).await.unwrap();
    {
        let rule = rule.lock().unwrap();
        assert_eq!(rule.as_ref().unwrap()["displayName"], crate::rules::RULE_NAME);
        assert_eq!(rule.as_ref().unwrap()["actions"]["moveToFolder"], "jk");
        assert_eq!(addresses(&rule), ["ads@x.test"]);
    }
    // Later ones are added to it, once.
    block(&["spam@y.test", "ADS@x.test"], true).await.unwrap();
    assert_eq!(addresses(&rule.lock().unwrap()), ["ads@x.test", "spam@y.test"]);
    // Unblocking a sender who isn't blocked changes nothing.
    let writes = |log: &Mutex<Vec<String>>| log.lock().unwrap().iter().filter(|r| !r.starts_with("GET")).count();
    let before = writes(&log);
    block(&["friend@z.test"], false).await.unwrap();
    assert_eq!(writes(&log), before);
    // Taking the last one off removes the rule.
    block(&["ads@x.test"], false).await.unwrap();
    assert_eq!(addresses(&rule.lock().unwrap()), ["spam@y.test"]);
    block(&["Spam@Y.test"], false).await.unwrap();
    assert!(rule.lock().unwrap().is_none());
}
