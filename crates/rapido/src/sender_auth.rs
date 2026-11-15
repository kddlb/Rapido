//! Deciding whether a message really comes from the domain in its `From`,
//! using the `Authentication-Results` header (RFC 8601) added by our own
//! receiving server. Used to gate per-sender trust such as auto-loading images.

use jmap_proto::mail::EmailHeader;

use crate::header_fields::unfold;

/// Authentication service identifiers we trust: Fastmail's receiving servers.
/// Results from any other authserv-id may have been written by the sender.
const TRUSTED_AUTHSERV: &[&str] = &["messagingengine.com", "fastmail.com"];

fn trusted(authserv: &str) -> bool {
    let a = authserv.trim().trim_end_matches('.').to_ascii_lowercase();
    TRUSTED_AUTHSERV
        .iter()
        .any(|t| a == *t || a.ends_with(&format!(".{t}")))
}

/// Remove `(comments)` (RFC 5322 CFWS), which may contain anything.
fn strip_comments(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut depth = 0usize;
    let mut escaped = false;
    for c in s.chars() {
        if escaped {
            escaped = false;
            if depth == 0 {
                out.push(c);
            }
            continue;
        }
        match c {
            '\\' => escaped = true,
            '(' => depth += 1,
            ')' if depth > 0 => depth -= 1,
            c if depth == 0 => out.push(c),
            _ => {}
        }
    }
    out
}

/// The domain part of an address, lowercased.
pub fn domain_of(address: &str) -> Option<String> {
    address
        .rsplit_once('@')
        .map(|(_, d)| d.trim().trim_end_matches('>').to_ascii_lowercase())
        .filter(|d| !d.is_empty())
}

/// Whether our receiving server recorded `dmarc=pass` for `from_domain`.
/// Only the topmost trusted `Authentication-Results` header counts; it is the
/// one our server added (it strips forged copies carrying its own id).
pub fn dmarc_pass_for(headers: &[EmailHeader], from_domain: &str) -> bool {
    let from_domain = from_domain.trim().to_ascii_lowercase();
    let Some(results) = headers
        .iter()
        .filter(|h| h.name.eq_ignore_ascii_case("Authentication-Results"))
        .map(|h| strip_comments(&unfold(&h.value)))
        .find(|v| {
            let authserv = v.split(';').next().unwrap_or("");
            // "authserv-id [version]"
            trusted(authserv.split_whitespace().next().unwrap_or(""))
        })
    else {
        return false;
    };
    results.split(';').skip(1).any(|resinfo| {
        let mut parts = resinfo.split_whitespace();
        let Some(method) = parts.next() else {
            return false;
        };
        method.eq_ignore_ascii_case("dmarc=pass")
            && parts.any(|p| {
                p.split_once('=').is_some_and(|(k, v)| {
                    k.eq_ignore_ascii_case("header.from")
                        && v.trim_matches('"').eq_ignore_ascii_case(&from_domain)
                })
            })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ar(value: &str) -> EmailHeader {
        EmailHeader {
            name: "Authentication-Results".into(),
            value: value.into(),
        }
    }

    const FASTMAIL: &str = " mx3.messagingengine.com;\r\n    dkim=pass (2048-bit rsa key sha256) header.d=example.com\r\n    header.i=@example.com header.b=abc header.a=rsa-sha256;\r\n    dmarc=pass policy.published-domain-policy=reject\r\n    policy.applied-disposition=none policy.evaluated-disposition=none\r\n    (p=reject,d=none,d.eval=none) policy.policy-from=p\r\n    header.from=example.com;\r\n    spf=pass smtp.mailfrom=bounce@example.com";

    #[test]
    fn fastmail_dmarc_pass() {
        assert!(dmarc_pass_for(&[ar(FASTMAIL)], "example.com"));
        assert!(dmarc_pass_for(&[ar(FASTMAIL)], "EXAMPLE.com"));
        assert!(!dmarc_pass_for(&[ar(FASTMAIL)], "evil.com"), "other domain");
    }

    #[test]
    fn untrusted_authserv_ignored() {
        let forged = ar(" mx.evil.com; dmarc=pass header.from=example.com");
        assert!(!dmarc_pass_for(
            std::slice::from_ref(&forged),
            "example.com"
        ));
        let lookalike = ar(" evilmessagingengine.com; dmarc=pass header.from=example.com");
        assert!(!dmarc_pass_for(&[lookalike], "example.com"));
    }

    #[test]
    fn only_topmost_trusted_header_counts() {
        // Our server says fail; a deeper (forged) header claiming pass is ignored.
        let ours = ar(" mx1.messagingengine.com; dmarc=fail header.from=example.com");
        let forged = ar(" mx9.messagingengine.com; dmarc=pass header.from=example.com");
        assert!(!dmarc_pass_for(&[ours, forged], "example.com"));
    }

    #[test]
    fn comments_cannot_smuggle_results() {
        let tricky = ar(
            " mx1.messagingengine.com; dmarc=fail (dmarc=pass header.from=example.com) header.from=example.com",
        );
        assert!(!dmarc_pass_for(&[tricky], "example.com"));
        assert!(!dmarc_pass_for(&[], "example.com"));
    }

    #[test]
    fn domains() {
        assert_eq!(
            domain_of("News@Example.COM").as_deref(),
            Some("example.com")
        );
        assert_eq!(domain_of("nodomain"), None);
    }
}
