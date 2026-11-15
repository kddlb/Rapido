//! JMAP Mail (RFC 8621) data types and methods.

use std::collections::HashMap;

use chrono::{DateTime, FixedOffset, Utc};
use serde::{Deserialize, Serialize};

use crate::error::string_enum;
use crate::method::*;
use crate::{Id, capability};

// ---------------------------------------------------------------------------
// Mailbox (RFC 8621 §2)

string_enum!(
    /// Mailbox role (IANA "IMAP Mailbox Name Attributes", lowercased).
    MailboxRole {
        All => "all",
        Archive => "archive",
        Drafts => "drafts",
        Flagged => "flagged",
        Important => "important",
        Inbox => "inbox",
        Junk => "junk",
        Sent => "sent",
        Subscribed => "subscribed",
        Trash => "trash",
    }
);

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Mailbox {
    pub id: Id,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<Id>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<MailboxRole>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sort_order: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_emails: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unread_emails: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_threads: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unread_threads: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub my_rights: Option<MailboxRights>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_subscribed: Option<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MailboxRights {
    pub may_read_items: bool,
    pub may_add_items: bool,
    pub may_remove_items: bool,
    pub may_set_seen: bool,
    pub may_set_keywords: bool,
    pub may_create_child: bool,
    pub may_rename: bool,
    pub may_delete: bool,
    pub may_submit: bool,
}

string_enum!(
    MailboxProperty {
        Id => "id",
        Name => "name",
        ParentId => "parentId",
        Role => "role",
        SortOrder => "sortOrder",
        TotalEmails => "totalEmails",
        UnreadEmails => "unreadEmails",
        TotalThreads => "totalThreads",
        UnreadThreads => "unreadThreads",
        MyRights => "myRights",
        IsSubscribed => "isSubscribed",
    }
);

// ---------------------------------------------------------------------------
// Thread (RFC 8621 §3)

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Thread {
    pub id: Id,
    /// Email ids in the thread, sorted by `receivedAt` (oldest first).
    #[serde(default)]
    pub email_ids: Vec<Id>,
}

string_enum!(
    ThreadProperty {
        Id => "id",
        EmailIds => "emailIds",
    }
);

// ---------------------------------------------------------------------------
// Email (RFC 8621 §4)

/// Well-known keywords (RFC 8621 §4.1.1).
pub mod keyword {
    pub const SEEN: &str = "$seen";
    pub const FLAGGED: &str = "$flagged";
    pub const DRAFT: &str = "$draft";
    pub const ANSWERED: &str = "$answered";
    pub const FORWARDED: &str = "$forwarded";
    pub const JUNK: &str = "$junk";
    pub const NOT_JUNK: &str = "$notjunk";
    pub const PHISHING: &str = "$phishing";
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmailAddress {
    #[serde(default)]
    pub name: Option<String>,
    pub email: String,
}

impl EmailAddress {
    /// Display name if present and non-empty, else the address.
    pub fn display(&self) -> &str {
        match &self.name {
            Some(n) if !n.trim().is_empty() => n,
            _ => &self.email,
        }
    }
}

/// An email. Every property except `id` is optional because `/get` returns only
/// what was asked for.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Email {
    pub id: Id,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blob_id: Option<Id>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<Id>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mailbox_ids: Option<HashMap<Id, bool>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keywords: Option<HashMap<String, bool>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub received_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_id: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub in_reply_to: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub references: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sender: Option<Vec<EmailAddress>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<Vec<EmailAddress>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<Vec<EmailAddress>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cc: Option<Vec<EmailAddress>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bcc: Option<Vec<EmailAddress>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_to: Option<Vec<EmailAddress>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sent_at: Option<DateTime<FixedOffset>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub has_attachment: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview: Option<String>,
    /// Every header field in message order, raw values (RFC 8621 §4.1.3).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub headers: Option<Vec<EmailHeader>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body_structure: Option<EmailBodyPart>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body_values: Option<HashMap<String, EmailBodyValue>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text_body: Option<Vec<EmailBodyPart>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub html_body: Option<Vec<EmailBodyPart>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attachments: Option<Vec<EmailBodyPart>>,
}

impl Email {
    pub fn has_keyword(&self, kw: &str) -> bool {
        self.keywords
            .as_ref()
            .is_some_and(|k| k.get(kw).copied().unwrap_or(false))
    }
    pub fn is_seen(&self) -> bool {
        self.has_keyword(keyword::SEEN)
    }
    pub fn is_flagged(&self) -> bool {
        self.has_keyword(keyword::FLAGGED)
    }
    pub fn in_mailbox(&self, mailbox: &Id) -> bool {
        self.mailbox_ids
            .as_ref()
            .is_some_and(|m| m.get(mailbox).copied().unwrap_or(false))
    }
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EmailBodyPart {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub part_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blob_id: Option<Id>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub headers: Option<Vec<EmailHeader>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(rename = "type", default, skip_serializing_if = "Option::is_none")]
    pub content_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub charset: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disposition: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cid: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sub_parts: Option<Vec<EmailBodyPart>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmailHeader {
    pub name: String,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EmailBodyValue {
    pub value: String,
    #[serde(default)]
    pub is_encoding_problem: bool,
    #[serde(default)]
    pub is_truncated: bool,
}

string_enum!(
    EmailProperty {
        Id => "id",
        BlobId => "blobId",
        ThreadId => "threadId",
        MailboxIds => "mailboxIds",
        Keywords => "keywords",
        Size => "size",
        ReceivedAt => "receivedAt",
        MessageId => "messageId",
        InReplyTo => "inReplyTo",
        References => "references",
        Sender => "sender",
        From => "from",
        To => "to",
        Cc => "cc",
        Bcc => "bcc",
        ReplyTo => "replyTo",
        Subject => "subject",
        SentAt => "sentAt",
        HasAttachment => "hasAttachment",
        Preview => "preview",
        Headers => "headers",
        BodyStructure => "bodyStructure",
        BodyValues => "bodyValues",
        TextBody => "textBody",
        HtmlBody => "htmlBody",
        Attachments => "attachments",
    }
);

string_enum!(
    EmailBodyProperty {
        PartId => "partId",
        BlobId => "blobId",
        Size => "size",
        Headers => "headers",
        Name => "name",
        Type => "type",
        Charset => "charset",
        Disposition => "disposition",
        Cid => "cid",
        Language => "language",
        Location => "location",
        SubParts => "subParts",
    }
);

string_enum!(
    EmailSortProperty {
        ReceivedAt => "receivedAt",
        Size => "size",
        From => "from",
        To => "to",
        Subject => "subject",
        SentAt => "sentAt",
        HasKeyword => "hasKeyword",
        AllInThreadHaveKeyword => "allInThreadHaveKeyword",
        SomeInThreadHaveKeyword => "someInThreadHaveKeyword",
    }
);

/// `Email/query` filter condition (RFC 8621 §4.4.1).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EmailFilterCondition {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub in_mailbox: Option<Id>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub in_mailbox_other_than: Option<Vec<Id>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_size: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_size: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub all_in_thread_have_keyword: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub some_in_thread_have_keyword: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub none_in_thread_have_keyword: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub has_keyword: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub not_keyword: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub has_attachment: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cc: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bcc: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub header: Option<Vec<String>>,
}

// ---------------------------------------------------------------------------
// Methods

macro_rules! method {
    ($ty:ty, $name:literal, $cap:expr, $resp:ty) => {
        impl Method for $ty {
            const NAME: &'static str = $name;
            const CAPABILITY: &'static str = $cap;
            type Response = $resp;
        }
    };
}

/// `Mailbox/get`.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(transparent)]
pub struct MailboxGet(pub Get<MailboxProperty>);
method!(
    MailboxGet,
    "Mailbox/get",
    capability::MAIL,
    GetResponse<Mailbox>
);

/// `Mailbox/changes`.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(transparent)]
pub struct MailboxChanges(pub Changes);
method!(
    MailboxChanges,
    "Mailbox/changes",
    capability::MAIL,
    ChangesResponse
);

/// `Thread/get`.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(transparent)]
pub struct ThreadGet(pub Get<ThreadProperty>);
method!(
    ThreadGet,
    "Thread/get",
    capability::MAIL,
    GetResponse<Thread>
);

/// `Thread/changes`.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(transparent)]
pub struct ThreadChanges(pub Changes);
method!(
    ThreadChanges,
    "Thread/changes",
    capability::MAIL,
    ChangesResponse
);

/// `Email/get` (RFC 8621 §4.2).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EmailGet {
    #[serde(flatten)]
    pub get: Get<EmailProperty>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body_properties: Option<Vec<EmailBodyProperty>>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub fetch_text_body_values: bool,
    #[serde(
        rename = "fetchHTMLBodyValues",
        skip_serializing_if = "std::ops::Not::not"
    )]
    pub fetch_html_body_values: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub fetch_all_body_values: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_body_value_bytes: Option<u64>,
}

impl EmailGet {
    pub fn new(get: Get<EmailProperty>) -> Self {
        Self {
            get,
            body_properties: None,
            fetch_text_body_values: false,
            fetch_html_body_values: false,
            fetch_all_body_values: false,
            max_body_value_bytes: None,
        }
    }
}
method!(EmailGet, "Email/get", capability::MAIL, GetResponse<Email>);

/// `Email/changes`.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(transparent)]
pub struct EmailChanges(pub Changes);
method!(
    EmailChanges,
    "Email/changes",
    capability::MAIL,
    ChangesResponse
);

/// `Email/query` (RFC 8621 §4.4).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EmailQuery {
    #[serde(flatten)]
    pub query: Query<EmailFilterCondition, EmailSortProperty>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub collapse_threads: bool,
}
method!(EmailQuery, "Email/query", capability::MAIL, QueryResponse);

/// `Email/queryChanges` (RFC 8621 §4.5).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EmailQueryChanges {
    #[serde(flatten)]
    pub query_changes: QueryChanges<EmailFilterCondition, EmailSortProperty>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub collapse_threads: bool,
}
method!(
    EmailQueryChanges,
    "Email/queryChanges",
    capability::MAIL,
    QueryChangesResponse
);

/// `Email/set` (RFC 8621 §4.6).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(transparent)]
pub struct EmailSet(pub Set<Email>);
method!(EmailSet, "Email/set", capability::MAIL, SetResponse<Email>);

/// Patch helpers for common `Email/set` updates.
pub mod patch {
    use serde_json::Value;

    use crate::{Id, PatchObject};

    /// Set or clear a keyword, e.g. `keywords/$seen`.
    pub fn keyword(p: &mut PatchObject, kw: &str, on: bool) {
        p.insert(
            format!("keywords/{}", escape(kw)),
            if on { Value::Bool(true) } else { Value::Null },
        );
    }

    /// Add or remove a mailbox membership.
    pub fn mailbox(p: &mut PatchObject, mailbox: &Id, member: bool) {
        p.insert(
            format!("mailboxIds/{}", escape(mailbox.as_str())),
            if member {
                Value::Bool(true)
            } else {
                Value::Null
            },
        );
    }

    /// Replace all mailbox memberships (a move).
    pub fn set_mailboxes<'a>(p: &mut PatchObject, mailboxes: impl IntoIterator<Item = &'a Id>) {
        let map = mailboxes
            .into_iter()
            .map(|id| (id.to_string(), Value::Bool(true)))
            .collect();
        p.insert("mailboxIds".into(), Value::Object(map));
    }

    /// JSON pointer escaping (RFC 6901): `~` → `~0`, `/` → `~1`.
    fn escape(s: &str) -> String {
        s.replace('~', "~0").replace('/', "~1")
    }
}
