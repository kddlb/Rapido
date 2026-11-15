//! Text for new-mail desktop notifications and the window title.

use rapido_store::EmailMeta;

/// Individual notifications for up to this many messages; a summary beyond.
const MAX_INDIVIDUAL: usize = 3;

/// Notification servers may interpret a small HTML subset in the body.
fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn sender(m: &EmailMeta) -> String {
    m.from
        .first()
        .map(|a| a.display().to_owned())
        .unwrap_or_else(|| "(no sender)".into())
}

fn subject(m: &EmailMeta) -> &str {
    if m.subject.is_empty() {
        "(no subject)"
    } else {
        &m.subject
    }
}

/// `(summary, body)` pairs for newly arrived messages.
pub fn for_new_mail(metas: &[EmailMeta]) -> Vec<(String, String)> {
    match metas.len() {
        0 => Vec::new(),
        n if n <= MAX_INDIVIDUAL => metas
            .iter()
            .map(|m| (sender(m), escape(subject(m))))
            .collect(),
        n => {
            let mut senders: Vec<String> = Vec::new();
            for m in metas {
                let s = sender(m);
                if !senders.contains(&s) {
                    senders.push(s);
                }
            }
            let shown = senders
                .iter()
                .take(3)
                .cloned()
                .collect::<Vec<_>>()
                .join(", ");
            let more = if senders.len() > 3 {
                format!(" and {} more", senders.len() - 3)
            } else {
                String::new()
            };
            vec![(
                format!("{n} new messages"),
                escape(&format!("From {shown}{more}")),
            )]
        }
    }
}

/// "Rápido — (3) user@example.com" with an Inbox unread count.
pub fn window_title(unread: u64, username: &str) -> String {
    if unread > 0 {
        format!("Rápido — ({unread}) {username}")
    } else {
        format!("Rápido — {username}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use jmap_proto::mail::EmailAddress;

    fn meta(name: &str, subject: &str) -> EmailMeta {
        EmailMeta {
            id: name.into(),
            thread_id: "t".into(),
            blob_id: None,
            mailbox_ids: vec![],
            keywords: vec![],
            received_at: Utc::now(),
            sent_at: None,
            subject: subject.into(),
            from: vec![EmailAddress {
                name: Some(name.into()),
                email: format!("{name}@x"),
            }],
            to: vec![],
            cc: vec![],
            reply_to: vec![],
            preview: String::new(),
            size: 0,
            has_attachment: false,
        }
    }

    #[test]
    fn individual_then_summary() {
        let one = for_new_mail(&[meta("Ann", "Lunch <today>?")]);
        assert_eq!(
            one,
            vec![("Ann".to_string(), "Lunch &lt;today&gt;?".to_string())]
        );
        let many: Vec<_> = ["A", "B", "A", "C", "D"]
            .iter()
            .map(|n| meta(n, "s"))
            .collect();
        let s = for_new_mail(&many);
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].0, "5 new messages");
        assert_eq!(s[0].1, "From A, B, C and 1 more");
        assert!(for_new_mail(&[]).is_empty());
        assert_eq!(for_new_mail(&[meta("Z", "")])[0].1, "(no subject)");
    }

    #[test]
    fn titles() {
        assert_eq!(window_title(0, "me@x"), "Rápido — me@x");
        assert_eq!(window_title(4, "me@x"), "Rápido — (4) me@x");
    }
}
