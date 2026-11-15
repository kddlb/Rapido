//! Display formatting for dates and addresses.

use chrono::{DateTime, Datelike, Local, Utc};
use jmap_proto::mail::EmailAddress;

/// Compact list date: time today, weekday this week, "Mar 3" this year, else "2023-03-03".
pub fn list_date(at: DateTime<Utc>, now: DateTime<Local>) -> String {
    let local = at.with_timezone(&Local);
    let days = now
        .date_naive()
        .signed_duration_since(local.date_naive())
        .num_days();
    if days == 0 {
        local.format("%H:%M").to_string()
    } else if (1..7).contains(&days) {
        local.format("%a").to_string()
    } else if local.year() == now.year() {
        local.format("%b %-d").to_string()
    } else {
        local.format("%Y-%m-%d").to_string()
    }
}

/// Full date for the reader header.
pub fn full_date(at: DateTime<Utc>) -> String {
    at.with_timezone(&Local)
        .format("%a, %-d %b %Y %H:%M")
        .to_string()
}

/// "Name <addr>" (or just the address).
pub fn address(a: &EmailAddress) -> String {
    match &a.name {
        Some(n) if !n.trim().is_empty() => format!("{} <{}>", n.trim(), a.email),
        _ => a.email.clone(),
    }
}

pub fn address_list(list: &[EmailAddress]) -> String {
    list.iter().map(address).collect::<Vec<_>>().join(", ")
}

/// Short sender names for a thread row: first names when there are several.
pub fn senders(list: &[EmailAddress]) -> String {
    match list {
        [] => "(no sender)".into(),
        [one] => one.display().to_owned(),
        many => many
            .iter()
            .map(|a| {
                a.display()
                    .split_whitespace()
                    .next()
                    .unwrap_or(&a.email)
                    .to_owned()
            })
            .collect::<Vec<_>>()
            .join(", "),
    }
}

/// Parse a JMAP search snippet (HTML-escaped, matches in `<mark>`) into plain
/// text and the byte ranges to highlight.
pub fn snippet_spans(snippet: &str) -> (String, Vec<std::ops::Range<usize>>) {
    let mut text = String::with_capacity(snippet.len());
    let mut ranges = Vec::new();
    let mut open: Option<usize> = None;
    let mut rest = snippet;
    while !rest.is_empty() {
        if let Some(r) = rest.strip_prefix("<mark>") {
            open.get_or_insert(text.len());
            rest = r;
        } else if let Some(r) = rest.strip_prefix("</mark>") {
            if let Some(start) = open.take()
                && start < text.len()
            {
                ranges.push(start..text.len());
            }
            rest = r;
        } else if rest.starts_with('&') {
            let entities = [
                ("&amp;", '&'),
                ("&lt;", '<'),
                ("&gt;", '>'),
                ("&quot;", '"'),
                ("&#39;", '\''),
                ("&apos;", '\''),
            ];
            match entities.iter().find(|(e, _)| rest.starts_with(e)) {
                Some((e, c)) => {
                    text.push(*c);
                    rest = &rest[e.len()..];
                }
                None => {
                    text.push('&');
                    rest = &rest[1..];
                }
            }
        } else {
            let c = rest.chars().next().expect("non-empty");
            if !c.is_control() {
                text.push(c);
            }
            rest = &rest[c.len_utf8()..];
        }
    }
    if let Some(start) = open
        && start < text.len()
    {
        ranges.push(start..text.len());
    }
    (text, ranges)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, TimeZone};

    #[test]
    fn list_dates() {
        let now = Local.with_ymd_and_hms(2026, 11, 15, 12, 0, 0).unwrap();
        let utc = |d: DateTime<Local>| d.with_timezone(&Utc);
        assert_eq!(list_date(utc(now - Duration::hours(2)), now), "10:00");
        assert_eq!(list_date(utc(now - Duration::days(2)), now), "Fri");
        assert_eq!(list_date(utc(now - Duration::days(30)), now), "Oct 16");
        assert_eq!(list_date(utc(now - Duration::days(400)), now), "2025-10-11");
    }

    #[test]
    fn sender_names() {
        let a = |n: Option<&str>, e: &str| EmailAddress {
            name: n.map(Into::into),
            email: e.into(),
        };
        assert_eq!(senders(&[a(Some("Joe Bloggs"), "j@x")]), "Joe Bloggs");
        assert_eq!(
            senders(&[a(Some("Joe Bloggs"), "j@x"), a(None, "k@y")]),
            "Joe, k@y"
        );
        assert_eq!(address(&a(Some(" Ann "), "a@x")), "Ann <a@x>");
        assert_eq!(senders(&[]), "(no sender)");
    }

    #[test]
    fn snippets() {
        let (t, r) = snippet_spans("The <mark>Fóo</mark> &amp; <mark>bar</mark>&lt;");
        assert_eq!(t, "The Fóo & bar<");
        assert_eq!(
            r.iter().map(|r| &t[r.clone()]).collect::<Vec<_>>(),
            ["Fóo", "bar"]
        );
        let (t, r) = snippet_spans("unclosed <mark>tail");
        assert_eq!(&t[r[0].clone()], "tail");
        assert_eq!(snippet_spans("a &bogus; b").0, "a &bogus; b");
    }
}
