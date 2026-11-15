//! Making link targets visible and safe to judge: hosts are shown in their ASCII
//! (punycode) form so look-alike internationalized domains stand out.

use url::{Host, Url};

/// A link as the user should see it before deciding to open it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkTarget {
    /// The URL to open (normalized, ASCII host).
    pub url: String,
    /// What to show: the URL with any IDN host in punycode.
    pub display: String,
    /// The host is internationalized (contained non-ASCII or `xn--` labels).
    pub idn: bool,
    /// Only http(s) and mailto links are opened.
    pub openable: bool,
}

pub fn inspect(raw: &str) -> LinkTarget {
    let raw = raw.trim();
    if let Some(addr) = strip_prefix_ci(raw, "mailto:") {
        return inspect_mailto(addr);
    }
    match Url::parse(raw) {
        Ok(u) => {
            let ascii = u.as_str().to_owned(); // the url crate serializes hosts as punycode
            let idn = has_idn_host(&u) || original_host_non_ascii(raw);
            let openable = matches!(u.scheme(), "http" | "https");
            LinkTarget {
                url: ascii.clone(),
                display: ascii,
                idn,
                openable,
            }
        }
        Err(_) => LinkTarget {
            url: raw.to_owned(),
            display: raw.escape_default().to_string(),
            idn: false,
            openable: false,
        },
    }
}

fn strip_prefix_ci<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    (s.len() >= prefix.len() && s[..prefix.len()].eq_ignore_ascii_case(prefix))
        .then(|| &s[prefix.len()..])
}

fn inspect_mailto(rest: &str) -> LinkTarget {
    let (addr, query) = rest
        .split_once('?')
        .map_or((rest, None), |(a, q)| (a, Some(q)));
    let (local, domain) = addr.rsplit_once('@').unwrap_or((addr, ""));
    let (ascii_domain, ok) = match Host::parse(domain) {
        Ok(h) => (h.to_string(), !domain.is_empty()),
        Err(_) => (domain.to_owned(), false),
    };
    let idn = !domain.is_ascii() || ascii_domain.split('.').any(|l| l.starts_with("xn--"));
    let mut url = format!("mailto:{local}@{ascii_domain}");
    if let Some(q) = query {
        url.push('?');
        url.push_str(q);
    }
    LinkTarget {
        display: url.clone(),
        url,
        idn,
        openable: ok && local.is_ascii(),
    }
}

fn has_idn_host(u: &Url) -> bool {
    matches!(u.host(), Some(Host::Domain(d)) if d.split('.').any(|l| l.starts_with("xn--")))
}

fn original_host_non_ascii(raw: &str) -> bool {
    let after_scheme = raw.split_once("://").map_or(raw, |(_, r)| r);
    let authority = after_scheme.split(['/', '?', '#']).next().unwrap_or("");
    !authority.is_ascii()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idn_hosts_become_punycode() {
        // Cyrillic "а" in place of Latin "a".
        let t = inspect("https://p\u{0430}ypal.com/login");
        assert!(t.idn);
        assert!(t.display.starts_with("https://xn--"), "{}", t.display);
        assert!(t.display.is_ascii());
        assert!(t.openable);
    }

    #[test]
    fn plain_hosts_unchanged() {
        let t = inspect("https://example.com/a?b=1");
        assert!(!t.idn);
        assert_eq!(t.display, "https://example.com/a?b=1");
    }

    #[test]
    fn already_punycode_is_flagged() {
        assert!(inspect("https://xn--80ak6aa92e.com/").idn);
    }

    #[test]
    fn mailto_domains() {
        let t = inspect("mailto:a@p\u{0430}ypal.com?subject=hi");
        assert!(t.idn);
        assert!(t.display.starts_with("mailto:a@xn--"));
        assert!(t.display.ends_with("?subject=hi"));
        assert!(t.openable);
        assert!(!inspect("mailto:a@example.com").idn);
    }

    #[test]
    fn other_schemes_not_openable() {
        assert!(!inspect("javascript:alert(1)").openable);
        assert!(!inspect("file:///etc/passwd").openable);
        assert!(!inspect("not a url").openable);
    }
}
