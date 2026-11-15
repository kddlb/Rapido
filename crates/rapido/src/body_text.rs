//! Turning message bodies into text for the native reader.

use rapido_store::EmailBody;

/// Text to show in the reader: the text/plain part if any, otherwise the HTML
/// part rendered to text. Returns `None` when the message has no body text.
pub fn reader_text(body: &EmailBody) -> Option<String> {
    if let Some(t) = body.plain_text() {
        return Some(normalize(&t));
    }
    body.html().map(|h| html_to_text(&h))
}

pub fn html_to_text(html: &str) -> String {
    let text = html2text::from_read(html.as_bytes(), 100).unwrap_or_else(|_| html.to_owned());
    normalize(&text)
}

/// Normalize line endings and collapse runs of more than two blank lines.
fn normalize(s: &str) -> String {
    let s = s.replace("\r\n", "\n").replace('\r', "\n");
    let mut out = String::with_capacity(s.len());
    let mut blank = 0;
    for line in s.lines() {
        let line = line.trim_end();
        if line.is_empty() {
            blank += 1;
            if blank > 2 {
                continue;
            }
        } else {
            blank = 0;
        }
        out.push_str(line);
        out.push('\n');
    }
    out.trim_matches('\n').to_owned()
}

/// Find http(s) and mailto links in text, as byte ranges, for clickable spans.
pub fn find_links(text: &str) -> Vec<(std::ops::Range<usize>, String)> {
    static RE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r#"(?i)\b(?:https?://|mailto:)[^\s<>"'\]\[)]+"#).unwrap()
    });
    RE.find_iter(text)
        .map(|m| {
            let s = m.as_str().trim_end_matches(['.', ',', ';', ':', '!', '?']);
            (m.start()..m.start() + s.len(), s.to_owned())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn html_becomes_text() {
        let t = html_to_text("<p>Hello <b>there</b></p><p>Second</p>");
        assert!(t.contains("Hello"));
        assert!(t.contains("Second"));
        assert!(!t.contains("<p>"));
    }

    #[test]
    fn collapses_blank_lines() {
        assert_eq!(normalize("a\r\n\r\n\r\n\r\n\r\nb  \n"), "a\n\n\nb");
    }

    #[test]
    fn links() {
        let l = find_links("see https://example.com/x?y=1. or mailto:a@b.c, ok");
        assert_eq!(l[0].1, "https://example.com/x?y=1");
        assert_eq!(l[1].1, "mailto:a@b.c");
        assert_eq!(
            &"see https://example.com/x?y=1. or"[l[0].0.clone()],
            "https://example.com/x?y=1"
        );
    }
}
