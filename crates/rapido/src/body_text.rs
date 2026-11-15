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

/// Build the minimal HTML the selectable reader renders: escaped text, one
/// `<p>` per paragraph, `<br>` per line, indentation and runs of spaces kept as
/// no-break spaces, and `links` (byte ranges into `text`) as anchors.
pub fn to_view_html(text: &str, links: &[(std::ops::Range<usize>, String)]) -> String {
    let mut out = String::with_capacity(text.len() + text.len() / 8);
    out.push_str("<p>");
    let mut pos = 0;
    let mut links = links.iter().peekable();
    let mut at_line_start = true;
    let mut prev_space = false;
    let mut newlines = 0usize;
    while pos < text.len() {
        if let Some((range, url)) = links.peek()
            && range.start == pos
        {
            flush_newlines(&mut out, &mut newlines);
            out.push_str("<a href=\"");
            escape_into(&mut out, url);
            out.push_str("\">");
            escape_into(&mut out, &text[range.clone()]);
            out.push_str("</a>");
            pos = range.end;
            links.next();
            at_line_start = false;
            prev_space = false;
            continue;
        }
        let c = text[pos..].chars().next().expect("in bounds");
        pos += c.len_utf8();
        match c {
            '\n' => {
                newlines += 1;
                at_line_start = true;
                prev_space = false;
            }
            ' ' => {
                flush_newlines(&mut out, &mut newlines);
                out.push(if at_line_start || prev_space {
                    '\u{a0}'
                } else {
                    ' '
                });
                prev_space = true;
            }
            '\t' => {
                flush_newlines(&mut out, &mut newlines);
                out.push_str("\u{a0}\u{a0}\u{a0}\u{a0}");
                prev_space = true;
            }
            c => {
                flush_newlines(&mut out, &mut newlines);
                escape_char(&mut out, c);
                at_line_start = false;
                prev_space = false;
            }
        }
    }
    out.push_str("</p>");
    out
}

fn flush_newlines(out: &mut String, n: &mut usize) {
    match *n {
        0 => {}
        1 => out.push_str("<br>"),
        _ => out.push_str("</p><p>"),
    }
    *n = 0;
}

fn escape_char(out: &mut String, c: char) {
    match c {
        '&' => out.push_str("&amp;"),
        '<' => out.push_str("&lt;"),
        '>' => out.push_str("&gt;"),
        '"' => out.push_str("&quot;"),
        c if c.is_control() => {}
        c => out.push(c),
    }
}

fn escape_into(out: &mut String, s: &str) {
    s.chars().for_each(|c| escape_char(out, c));
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

    #[test]
    fn view_html_escapes_and_links() {
        let text = "Hi <b>&\n  indented\n\nsee https://x.com/?a=1&b=2 now";
        let links = find_links(text);
        let html = to_view_html(text, &links);
        assert_eq!(
            html,
            "<p>Hi &lt;b&gt;&amp;<br>\u{a0}\u{a0}indented</p><p>see \
             <a href=\"https://x.com/?a=1&amp;b=2\">https://x.com/?a=1&amp;b=2</a> now</p>"
        );
    }

    #[test]
    fn view_html_keeps_runs_of_spaces() {
        assert_eq!(
            to_view_html("a  b\tc", &[]),
            "<p>a \u{a0}b\u{a0}\u{a0}\u{a0}\u{a0}c</p>"
        );
        assert_eq!(to_view_html("", &[]), "<p></p>");
    }

    #[test]
    fn view_html_drops_control_chars() {
        assert_eq!(to_view_html("a\u{0}b\u{1b}c", &[]), "<p>abc</p>");
    }
}
