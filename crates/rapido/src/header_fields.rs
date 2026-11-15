//! The structured header view: unfolded, decoded values in a table, with
//! public IP addresses linked to ipinfo.io.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::sync::LazyLock;

use jmap_proto::mail::EmailHeader;
use regex::Regex;

/// Join folded lines (RFC 5322 §2.2.3) and trim.
pub fn unfold(raw: &str) -> String {
    static FOLD: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\r?\n[ \t]+").unwrap());
    FOLD.replace_all(raw, " ").trim().to_owned()
}

/// Decode RFC 2047 encoded words; falls back to the input on any error.
pub fn decode(value: &str) -> String {
    if !value.contains("=?") {
        return value.to_owned();
    }
    rfc2047_decoder::decode(value.as_bytes()).unwrap_or_else(|_| value.to_owned())
}

/// A display value: unfolded, decoded, control characters removed.
pub fn display_value(raw: &str) -> String {
    decode(&unfold(raw))
        .chars()
        .filter(|c| !c.is_control() || *c == '\t')
        .collect()
}

static IP_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?x)
        (?P<v4>\b(?:\d{1,3}\.){3}\d{1,3}\b)
        | (?:^|[^0-9A-Za-z:.]|IPv6:)   # IPv6 starts at a boundary (no look-behind in `regex`)
          (?P<v6>(?:[0-9A-Fa-f]{0,4}:){2,7}(?:[0-9A-Fa-f]{1,4}|(?:\d{1,3}\.){3}\d{1,3})?)
        ",
    )
    .unwrap()
});

/// Whether an address is worth looking up (not private, loopback, link-local…).
pub fn is_public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_public_v4(v4),
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_public_v4(v4);
            }
            let seg0 = v6.segments()[0];
            !(v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                || (seg0 & 0xfe00) == 0xfc00 // unique local
                || (seg0 & 0xffc0) == 0xfe80 // link-local
                || (seg0 == 0x2001 && v6.segments()[1] == 0x0db8)) // documentation
        }
    }
}

fn is_public_v4(v4: Ipv4Addr) -> bool {
    let [a, b, ..] = v4.octets();
    !(v4.is_private()
        || v4.is_loopback()
        || v4.is_link_local()
        || v4.is_unspecified()
        || v4.is_broadcast()
        || v4.is_multicast()
        || v4.is_documentation()
        || (a == 100 && (64..128).contains(&b)) // CGNAT
        || a == 0
        || a >= 240)
}

/// Public IP addresses in `text`, as byte ranges.
pub fn find_ips(text: &str) -> Vec<(std::ops::Range<usize>, IpAddr)> {
    IP_RE
        .captures_iter(text)
        .filter_map(|c| {
            let m = c.name("v4").or_else(|| c.name("v6"))?;
            let ip: IpAddr = if c.name("v4").is_some() {
                IpAddr::V4(m.as_str().parse::<Ipv4Addr>().ok()?)
            } else {
                IpAddr::V6(m.as_str().parse::<Ipv6Addr>().ok()?)
            };
            is_public(ip).then(|| (m.range(), ip))
        })
        .collect()
}

pub fn ipinfo_url(ip: IpAddr) -> String {
    format!("https://ipinfo.io/{ip}")
}

fn escape(s: &str, out: &mut String) {
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            c => out.push(c),
        }
    }
}

fn linkify_ips(value: &str, out: &mut String) {
    let mut pos = 0;
    for (range, ip) in find_ips(value) {
        escape(&value[pos..range.start], out);
        out.push_str("<a href=\"");
        escape(&ipinfo_url(ip), out);
        out.push_str("\">");
        escape(&value[range.clone()], out);
        out.push_str("</a>");
        pos = range.end;
    }
    escape(&value[pos..], out);
}

/// The header table for the selectable HTML text view.
pub fn table_html(headers: &[EmailHeader]) -> String {
    let mut out =
        String::from("<table><thead><tr><th>Field</th><th>Value</th></tr></thead><tbody>");
    for h in headers {
        out.push_str("<tr><td><b>");
        escape(&h.name, &mut out);
        out.push_str("</b></td><td>");
        linkify_ips(&display_value(&h.value), &mut out);
        out.push_str("</td></tr>");
    }
    out.push_str("</tbody></table>");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unfolds_and_decodes() {
        assert_eq!(
            unfold(" from a\r\n\tby b\r\n  with c "),
            "from a by b with c"
        );
        assert_eq!(
            display_value(" =?UTF-8?B?UsOhcGlkbw==?= rocks"),
            "Rápido rocks"
        );
        assert_eq!(display_value(" =?ISO-8859-1?Q?caf=E9?="), "café");
        assert_eq!(display_value(" =?bogus"), "=?bogus");
    }

    #[test]
    fn finds_public_ips_only() {
        let text = "from mx (mx.example [203.0.113.5]) by [8.8.8.8] via 10.0.0.1, 127.0.0.1 \
                    and [IPv6:2606:4700:4700::1111] and fe80::1 and 2001:db8::1, version 1.2.3";
        let ips: Vec<String> = find_ips(text)
            .into_iter()
            .map(|(_, ip)| ip.to_string())
            .collect();
        assert_eq!(ips, ["8.8.8.8", "2606:4700:4700::1111"]);
    }

    #[test]
    fn ignores_non_ips() {
        assert!(find_ips("999.1.1.1 and 12:30:45 and a::b::c").is_empty());
    }

    #[test]
    fn table_links_ips_and_escapes() {
        let html = table_html(&[
            EmailHeader {
                name: "Received".into(),
                value: " from <x> [1.1.1.1]".into(),
            },
            EmailHeader {
                name: "X-<b>".into(),
                value: " v".into(),
            },
        ]);
        assert!(
            html.contains(r#"<a href="https://ipinfo.io/1.1.1.1">1.1.1.1</a>"#),
            "{html}"
        );
        assert!(html.contains("from &lt;x&gt;"));
        assert!(html.contains("<b>X-&lt;b&gt;</b>"));
    }
}
