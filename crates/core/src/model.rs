//! What the cache holds, and the shapes Graph sends it in.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Address {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub address: String,
}

impl Address {
    /// The name, or the address when there is no name.
    pub fn display(&self) -> &str {
        if self.name.trim().is_empty() { &self.address } else { &self.name }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Folder {
    pub id: String,
    pub parent_id: Option<String>,
    pub name: String,
    /// `inbox`, `sentitems`, `drafts`, `deleteditems`, `junkemail`,
    /// `archive` or `outbox` for the folders every mailbox has.
    pub well_known: Option<String>,
    pub unread: u32,
    pub total: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Flag {
    #[default]
    None,
    Flagged,
    Complete,
}

impl Flag {
    pub fn from_graph(status: &str) -> Self {
        match status {
            "flagged" => Flag::Flagged,
            "complete" => Flag::Complete,
            _ => Flag::None,
        }
    }

    pub fn to_graph(self) -> &'static str {
        match self {
            Flag::None => "notFlagged",
            Flag::Flagged => "flagged",
            Flag::Complete => "complete",
        }
    }

    pub fn from_i64(value: i64) -> Self {
        match value {
            1 => Flag::Flagged,
            2 => Flag::Complete,
            _ => Flag::None,
        }
    }

    pub fn to_i64(self) -> i64 {
        match self {
            Flag::None => 0,
            Flag::Flagged => 1,
            Flag::Complete => 2,
        }
    }
}

/// A message as the list shows it.
#[derive(Debug, Clone, PartialEq)]
pub struct MessageSummary {
    pub account: String,
    pub id: String,
    pub folder_id: String,
    pub conversation_id: String,
    pub subject: String,
    pub from: Address,
    pub to: Vec<Address>,
    pub preview: String,
    /// Seconds since the Unix epoch, UTC.
    pub received: i64,
    pub is_read: bool,
    pub is_draft: bool,
    pub flag: Flag,
    pub high_importance: bool,
    pub has_attachments: bool,
    /// In the Focused part of the Inbox rather than Other, as Microsoft's
    /// sorting or the user put it.
    pub focused: bool,
}

/// A message opened in the reading pane.
#[derive(Debug, Clone, PartialEq)]
pub struct MessageDetail {
    pub summary: MessageSummary,
    pub cc: Vec<Address>,
    pub bcc: Vec<Address>,
    pub reply_to: Vec<Address>,
    pub sent: i64,
    pub categories: Vec<String>,
    /// `None` until the body has been fetched.
    pub body_html: Option<String>,
    pub attachments: Vec<Attachment>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attachment {
    pub id: String,
    pub name: String,
    pub content_type: String,
    pub size: u64,
    pub is_inline: bool,
    pub content_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Calendar {
    pub id: String,
    pub name: String,
    /// `#rrggbb`, or empty for the default colour.
    pub color: String,
    pub can_edit: bool,
    pub is_default: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Response {
    #[default]
    None,
    Organizer,
    Accepted,
    Tentative,
    Declined,
    NotResponded,
}

impl Response {
    pub fn from_graph(value: &str) -> Self {
        match value {
            "organizer" => Response::Organizer,
            "accepted" => Response::Accepted,
            "tentativelyAccepted" => Response::Tentative,
            "declined" => Response::Declined,
            "notResponded" => Response::NotResponded,
            _ => Response::None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Response::None => "none",
            Response::Organizer => "organizer",
            Response::Accepted => "accepted",
            Response::Tentative => "tentativelyAccepted",
            Response::Declined => "declined",
            Response::NotResponded => "notResponded",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attendee {
    pub address: Address,
    pub response: Response,
    /// `required`, `optional` or `resource`.
    pub kind: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    pub account: String,
    pub id: String,
    pub calendar_id: String,
    pub subject: String,
    /// UTC seconds; for all-day events, midnight UTC of the first day.
    pub start: i64,
    /// Exclusive; for all-day events, midnight UTC after the last day.
    pub end: i64,
    pub all_day: bool,
    pub location: String,
    pub organizer: Address,
    pub attendees: Vec<Attendee>,
    pub preview: String,
    pub body_html: String,
    pub response: Response,
    /// `free`, `tentative`, `busy`, `oof`, `workingElsewhere`.
    pub show_as: String,
    pub is_cancelled: bool,
    pub is_online: bool,
    pub join_url: String,
    pub is_recurring: bool,
    pub web_link: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Phone {
    /// `mobile`, `business` or `home`.
    pub kind: String,
    pub number: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Contact {
    pub account: String,
    pub id: String,
    pub display_name: String,
    pub given_name: String,
    pub surname: String,
    pub emails: Vec<Address>,
    pub phones: Vec<Phone>,
    pub company: String,
    pub job_title: String,
    pub notes: String,
}

/// Graph's JSON shapes.
pub mod wire {
    use serde::{Deserialize, Serialize};

    #[derive(Debug, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub struct MailFolder {
        pub id: String,
        pub display_name: String,
        pub parent_folder_id: Option<String>,
        #[serde(default)]
        pub child_folder_count: u32,
        #[serde(default)]
        pub unread_item_count: u32,
        #[serde(default)]
        pub total_item_count: u32,
    }

    #[derive(Debug, Default, Deserialize, Serialize, Clone)]
    pub struct EmailAddress {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub name: Option<String>,
        #[serde(default)]
        pub address: Option<String>,
    }

    #[derive(Debug, Default, Deserialize, Serialize, Clone)]
    #[serde(rename_all = "camelCase")]
    pub struct Recipient {
        #[serde(default)]
        pub email_address: EmailAddress,
    }

    impl From<&Recipient> for super::Address {
        fn from(recipient: &Recipient) -> Self {
            super::Address {
                name: recipient.email_address.name.clone().unwrap_or_default(),
                address: recipient.email_address.address.clone().unwrap_or_default(),
            }
        }
    }

    impl From<&super::Address> for Recipient {
        fn from(address: &super::Address) -> Self {
            Recipient {
                email_address: EmailAddress {
                    name: (!address.name.is_empty()).then(|| address.name.clone()),
                    address: Some(address.address.clone()),
                },
            }
        }
    }

    #[derive(Debug, Default, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub struct FollowupFlag {
        #[serde(default)]
        pub flag_status: String,
    }

    #[derive(Debug, Deserialize)]
    pub struct Removed {
        #[serde(default)]
        pub reason: String,
    }

    #[derive(Debug, Default, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub struct ItemBody {
        #[serde(default)]
        pub content_type: String,
        #[serde(default)]
        pub content: String,
    }

    /// A message from a list, a delta page or a single fetch.
    #[derive(Debug, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub struct Message {
        pub id: String,
        #[serde(rename = "@removed")]
        pub removed: Option<Removed>,
        pub parent_folder_id: Option<String>,
        pub conversation_id: Option<String>,
        pub subject: Option<String>,
        pub body_preview: Option<String>,
        pub from: Option<Recipient>,
        pub to_recipients: Option<Vec<Recipient>>,
        pub cc_recipients: Option<Vec<Recipient>>,
        pub bcc_recipients: Option<Vec<Recipient>>,
        pub reply_to: Option<Vec<Recipient>>,
        pub received_date_time: Option<String>,
        pub sent_date_time: Option<String>,
        pub is_read: Option<bool>,
        pub is_draft: Option<bool>,
        pub flag: Option<FollowupFlag>,
        pub importance: Option<String>,
        pub has_attachments: Option<bool>,
        pub categories: Option<Vec<String>>,
        pub body: Option<ItemBody>,
        pub attachments: Option<Vec<AttachmentInfo>>,
        /// `focused` or `other`.
        pub inference_classification: Option<String>,
    }

    /// The fields fetched for the list; delta queries must keep asking for
    /// the same ones.
    pub const MESSAGE_FIELDS: &str = "id,parentFolderId,conversationId,subject,bodyPreview,from,\
        toRecipients,ccRecipients,replyTo,receivedDateTime,sentDateTime,isRead,isDraft,flag,\
        importance,hasAttachments,categories,inferenceClassification";

    #[derive(Debug, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub struct AttachmentInfo {
        pub id: String,
        #[serde(default)]
        pub name: Option<String>,
        #[serde(default)]
        pub content_type: Option<String>,
        #[serde(default)]
        pub size: u64,
        #[serde(default)]
        pub is_inline: bool,
        #[serde(default)]
        pub content_id: Option<String>,
        #[serde(rename = "@odata.type", default)]
        pub odata_type: String,
    }

    #[derive(Debug, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub struct Calendar {
        pub id: String,
        pub name: String,
        #[serde(default)]
        pub hex_color: Option<String>,
        #[serde(default)]
        pub can_edit: bool,
        #[serde(default)]
        pub is_default_calendar: bool,
    }

    #[derive(Debug, Default, Deserialize, Serialize, Clone)]
    #[serde(rename_all = "camelCase")]
    pub struct DateTimeTimeZone {
        pub date_time: String,
        pub time_zone: String,
    }

    #[derive(Debug, Default, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub struct Location {
        #[serde(default)]
        pub display_name: Option<String>,
    }

    #[derive(Debug, Default, Deserialize)]
    pub struct ResponseStatus {
        #[serde(default)]
        pub response: String,
    }

    #[derive(Debug, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub struct Attendee {
        #[serde(default)]
        pub email_address: EmailAddress,
        #[serde(default)]
        pub status: Option<ResponseStatus>,
        #[serde(rename = "type", default)]
        pub kind: Option<String>,
    }

    #[derive(Debug, Default, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub struct OnlineMeeting {
        #[serde(default)]
        pub join_url: Option<String>,
    }

    #[derive(Debug, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub struct Event {
        pub id: String,
        #[serde(rename = "@removed")]
        pub removed: Option<Removed>,
        pub subject: Option<String>,
        pub start: Option<DateTimeTimeZone>,
        pub end: Option<DateTimeTimeZone>,
        pub is_all_day: Option<bool>,
        pub location: Option<Location>,
        pub organizer: Option<Recipient>,
        pub attendees: Option<Vec<Attendee>>,
        pub body_preview: Option<String>,
        pub body: Option<ItemBody>,
        pub response_status: Option<ResponseStatus>,
        pub show_as: Option<String>,
        pub is_cancelled: Option<bool>,
        pub is_online_meeting: Option<bool>,
        pub online_meeting: Option<OnlineMeeting>,
        #[serde(rename = "type")]
        pub kind: Option<String>,
        pub series_master_id: Option<String>,
        pub web_link: Option<String>,
        /// Set by `calendarView` on the instances of each calendar.
        pub calendar: Option<CalendarRef>,
    }

    #[derive(Debug, Deserialize)]
    pub struct CalendarRef {
        pub id: String,
    }

    #[derive(Debug, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub struct Contact {
        pub id: String,
        #[serde(rename = "@removed")]
        pub removed: Option<Removed>,
        pub display_name: Option<String>,
        pub given_name: Option<String>,
        pub surname: Option<String>,
        pub email_addresses: Option<Vec<EmailAddress>>,
        pub mobile_phone: Option<String>,
        pub business_phones: Option<Vec<String>>,
        pub home_phones: Option<Vec<String>>,
        pub company_name: Option<String>,
        pub job_title: Option<String>,
        pub personal_notes: Option<String>,
    }
}
