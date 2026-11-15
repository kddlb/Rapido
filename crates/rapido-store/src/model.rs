use std::collections::HashMap;

use chrono::{DateTime, FixedOffset, Utc};
use jmap_proto::Id;
use jmap_proto::mail::{EmailAddress, EmailBodyPart, EmailBodyValue, MailboxRights, MailboxRole};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq)]
pub struct MailboxRow {
    pub id: Id,
    pub name: String,
    pub parent_id: Option<Id>,
    pub role: Option<MailboxRole>,
    pub sort_order: u32,
    pub total_emails: u64,
    pub unread_emails: u64,
    pub total_threads: u64,
    pub unread_threads: u64,
    pub rights: Option<MailboxRights>,
    pub is_subscribed: bool,
    /// Nesting depth, filled in by [`mailbox_tree`].
    pub depth: usize,
}

/// Order mailboxes depth-first by `sortOrder` then name (RFC 8621 §2), setting `depth`.
/// Orphans (parent missing) are treated as top-level.
pub fn mailbox_tree(mut rows: Vec<MailboxRow>) -> Vec<MailboxRow> {
    rows.sort_by(|a, b| {
        a.sort_order
            .cmp(&b.sort_order)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    let ids: std::collections::HashSet<Id> = rows.iter().map(|r| r.id.clone()).collect();
    let mut children: HashMap<Option<Id>, Vec<MailboxRow>> = HashMap::new();
    for r in rows {
        let parent = r.parent_id.clone().filter(|p| ids.contains(p));
        children.entry(parent).or_default().push(r);
    }
    let mut out = Vec::new();
    fn walk(
        parent: Option<Id>,
        depth: usize,
        children: &mut HashMap<Option<Id>, Vec<MailboxRow>>,
        out: &mut Vec<MailboxRow>,
    ) {
        for mut r in children.remove(&parent).unwrap_or_default() {
            r.depth = depth;
            let id = r.id.clone();
            out.push(r);
            walk(Some(id), depth + 1, children, out);
        }
    }
    walk(None, 0, &mut children, &mut out);
    out
}

/// One row in a mailbox's message list: a thread, represented by its newest
/// email in that mailbox.
#[derive(Debug, Clone, PartialEq)]
pub struct ThreadRow {
    pub email_id: Id,
    pub thread_id: Id,
    pub subject: String,
    /// Distinct senders in the thread, oldest first (at most 3).
    pub senders: Vec<EmailAddress>,
    pub preview: String,
    pub received_at: DateTime<Utc>,
    pub unread: bool,
    pub flagged: bool,
    pub count: usize,
    pub has_attachment: bool,
}

/// Stored metadata for one email.
#[derive(Debug, Clone, PartialEq)]
pub struct EmailMeta {
    pub id: Id,
    pub thread_id: Id,
    pub blob_id: Option<Id>,
    pub mailbox_ids: Vec<Id>,
    pub keywords: Vec<String>,
    pub received_at: DateTime<Utc>,
    pub sent_at: Option<DateTime<FixedOffset>>,
    pub subject: String,
    pub from: Vec<EmailAddress>,
    pub to: Vec<EmailAddress>,
    pub cc: Vec<EmailAddress>,
    pub reply_to: Vec<EmailAddress>,
    pub preview: String,
    pub size: u64,
    pub has_attachment: bool,
}

impl EmailMeta {
    pub fn has_keyword(&self, kw: &str) -> bool {
        self.keywords.iter().any(|k| k == kw)
    }
    pub fn is_seen(&self) -> bool {
        self.has_keyword(jmap_proto::mail::keyword::SEEN)
    }
    pub fn is_flagged(&self) -> bool {
        self.has_keyword(jmap_proto::mail::keyword::FLAGGED)
    }
}

/// A fetched message body.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct EmailBody {
    pub text_body: Vec<EmailBodyPart>,
    pub html_body: Vec<EmailBodyPart>,
    pub attachments: Vec<EmailBodyPart>,
    pub body_values: HashMap<String, EmailBodyValue>,
}

impl EmailBody {
    fn join(&self, parts: &[EmailBodyPart], ty: &str) -> Option<String> {
        let texts: Vec<&str> = parts
            .iter()
            .filter(|p| {
                p.content_type
                    .as_deref()
                    .is_some_and(|t| t.eq_ignore_ascii_case(ty))
            })
            .filter_map(|p| p.part_id.as_ref())
            .filter_map(|id| self.body_values.get(id))
            .map(|v| v.value.as_str())
            .collect();
        (!texts.is_empty()).then(|| texts.join("\n"))
    }

    /// The `text/plain` body, if the message has one.
    pub fn plain_text(&self) -> Option<String> {
        self.join(&self.text_body, "text/plain")
    }

    /// The `text/html` body, if the message has one.
    pub fn html(&self) -> Option<String> {
        self.join(&self.html_body, "text/html")
    }

    /// Inline parts referenced by `cid:` (Content-ID without angle brackets → part).
    pub fn inline_parts(&self) -> impl Iterator<Item = (&str, &EmailBodyPart)> {
        self.attachments
            .iter()
            .chain(self.html_body.iter())
            .filter_map(|p| {
                let cid = p.cid.as_deref()?;
                Some((cid.trim_start_matches('<').trim_end_matches('>'), p))
            })
    }
}

/// A cached `Email/query` result for one mailbox (a prefix of the full list).
#[derive(Debug, Clone, PartialEq)]
pub struct QueryCache {
    pub query_state: String,
    pub ids: Vec<Id>,
    pub total: Option<u64>,
}

/// What changed in the store, for the UI to refresh.
#[derive(Debug, Clone, PartialEq)]
pub enum StoreEvent {
    MailboxesChanged,
    /// A mailbox's message list changed.
    ListChanged(Id),
    /// Metadata (flags, membership) of these emails changed.
    EmailsChanged(Vec<Id>),
    BodyLoaded(Id),
    /// Unread mail that just arrived in the Inbox (from delta sync only, so a
    /// first sync or full resync doesn't announce the whole mailbox).
    NewMail(Vec<Id>),
    Online,
    Offline,
    AuthFailed,
    Error(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mb(id: &str, name: &str, parent: Option<&str>, order: u32) -> MailboxRow {
        MailboxRow {
            id: id.into(),
            name: name.into(),
            parent_id: parent.map(Id::from),
            role: None,
            sort_order: order,
            total_emails: 0,
            unread_emails: 0,
            total_threads: 0,
            unread_threads: 0,
            rights: None,
            is_subscribed: true,
            depth: 0,
        }
    }

    #[test]
    fn tree_orders_depth_first() {
        let rows = vec![
            mb("c", "Child", Some("p"), 0),
            mb("i", "Inbox", None, 1),
            mb("p", "Projects", None, 10),
            mb("a", "Archive", None, 10),
            mb("o", "Orphan", Some("missing"), 5),
        ];
        let tree = mailbox_tree(rows);
        let got: Vec<(&str, usize)> = tree.iter().map(|r| (r.id.as_str(), r.depth)).collect();
        assert_eq!(got, [("i", 0), ("o", 0), ("a", 0), ("p", 0), ("c", 1)]);
    }

    #[test]
    fn body_text_selection() {
        let part = |id: &str, ty: &str| EmailBodyPart {
            part_id: Some(id.into()),
            content_type: Some(ty.into()),
            ..Default::default()
        };
        let val = |v: &str| EmailBodyValue {
            value: v.into(),
            is_encoding_problem: false,
            is_truncated: false,
        };
        let b = EmailBody {
            text_body: vec![part("1", "text/plain")],
            html_body: vec![part("2", "TEXT/HTML")],
            attachments: vec![EmailBodyPart {
                cid: Some("<img1@x>".into()),
                ..Default::default()
            }],
            body_values: [("1".into(), val("plain")), ("2".into(), val("<b>html</b>"))].into(),
        };
        assert_eq!(b.plain_text().as_deref(), Some("plain"));
        assert_eq!(b.html().as_deref(), Some("<b>html</b>"));
        assert_eq!(b.inline_parts().next().unwrap().0, "img1@x");
    }
}
