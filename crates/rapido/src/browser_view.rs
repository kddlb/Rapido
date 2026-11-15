//! "Open in browser": sanitize a message's HTML and write it to a private temp
//! file that the system browser opens.
//!
//! Defence in depth:
//! 1. `ammonia` strips scripts, event handlers, `javascript:` URLs, forms,
//!    iframes, objects, `<meta>`/`<link>`/`<base>`, and comments.
//! 2. A Content-Security-Policy `<meta>` forbids scripts, frames, forms and any
//!    network fetch unless the user chose to load remote content.
//! 3. `cid:` images are inlined as `data:` URIs so nothing needs the network.
//! 4. The file is 0600 inside a 0700 directory under `$XDG_RUNTIME_DIR`.

use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use anyhow::{Context, Result};
use base64::Engine;
use regex::Regex;

/// Header lines shown above the message.
#[derive(Debug, Clone, Default)]
pub struct Header {
    pub from: String,
    pub to: String,
    pub subject: String,
    pub date: String,
}

/// An inline part referenced by `cid:`.
#[derive(Debug, Clone)]
pub struct InlinePart {
    pub mime: String,
    pub bytes: Vec<u8>,
}

/// Wording for the remote-content consent dialog.
pub const REMOTE_TITLE: &str = "Load remote images?";
pub const REMOTE_MESSAGE: &str = "Images hosted elsewhere are fetched from the sender's servers when the \
message opens. That can tell the sender that you opened this email and when, your IP address \
(and so your approximate location), and which browser and device you use. Many senders use \
this to track you.";

fn sanitizer() -> ammonia::Builder<'static> {
    let mut b = ammonia::Builder::default();
    b.add_tags(["style", "center", "font"])
        .rm_clean_content_tags(["style"])
        .add_generic_attributes([
            "style",
            "class",
            "align",
            "valign",
            "bgcolor",
            "width",
            "height",
            "dir",
            "border",
            "cellpadding",
            "cellspacing",
            "color",
            "face",
            "size",
        ])
        .add_tag_attributes("img", ["src", "alt", "title", "width", "height"])
        .url_schemes(HashSet::from(["http", "https", "mailto", "cid", "tel"]))
        .link_rel(Some("noopener noreferrer"))
        .strip_comments(true);
    b
}

/// Sanitize message HTML. Safe to embed in our template.
pub fn sanitize(html: &str) -> String {
    sanitizer().clean(html).to_string()
}

static REMOTE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"(?ix)
        \b(?:src|background|poster)\s*=\s*["']?\s*(?:https?:)?//   # src="https://… or //…
        | url\(\s*["']?\s*(?:https?:)?//                         # CSS url(https://…)
        | @import                                                # CSS imports
        "#,
    )
    .unwrap()
});

/// Whether sanitized HTML would fetch anything from the network if allowed.
pub fn has_remote_content(sanitized: &str) -> bool {
    REMOTE_RE.is_match(sanitized)
}

static CID_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?i)(["'(])cid:([^"')\s]+)"#).unwrap());

/// Replace `cid:` references with `data:` URIs for the parts we have.
pub fn inline_cids(html: &str, parts: &HashMap<String, InlinePart>) -> String {
    CID_RE
        .replace_all(html, |c: &regex::Captures| {
            let quote = &c[1];
            let cid = percent_decode(&c[2]);
            match parts.get(cid.trim_start_matches('<').trim_end_matches('>')) {
                Some(p) if is_image_mime(&p.mime) => format!(
                    "{quote}data:{};base64,{}",
                    p.mime,
                    base64::engine::general_purpose::STANDARD.encode(&p.bytes)
                ),
                _ => format!("{quote}about:blank"),
            }
        })
        .into_owned()
}

fn is_image_mime(m: &str) -> bool {
    let m = m.to_ascii_lowercase();
    m.starts_with("image/") && m != "image/svg+xml"
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let Ok(hex) = std::str::from_utf8(&bytes[i + 1..i + 3])
            && let Ok(v) = u8::from_str_radix(hex, 16)
        {
            out.push(v);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

static HREF_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"<a href="([^"]*)""#).unwrap());

/// Give every link a `title` with its real destination, host in punycode, so
/// hovering shows where it goes even when the link text says otherwise.
pub fn annotate_links(sanitized: &str) -> String {
    HREF_RE
        .replace_all(sanitized, |c: &regex::Captures| {
            let href = c[1].replace("&amp;", "&");
            let t = crate::links::inspect(&href);
            let note = if t.idn {
                " (internationalized domain)"
            } else {
                ""
            };
            let href = if t.openable {
                esc(&t.url)
            } else {
                c[1].to_owned()
            };
            format!(r#"<a title="{}{note}" href="{href}""#, esc(&t.display))
        })
        .into_owned()
}

/// The Content-Security-Policy for the generated page.
pub fn csp(allow_remote: bool) -> String {
    let remote = if allow_remote { " https: http:" } else { "" };
    format!(
        "default-src 'none'; script-src 'none'; object-src 'none'; frame-src 'none'; \
         child-src 'none'; worker-src 'none'; connect-src 'none'; form-action 'none'; \
         base-uri 'none'; style-src 'unsafe-inline'{remote}; font-src data:{remote}; \
         img-src data:{remote}; media-src 'none'"
    )
}

fn esc(s: &str) -> String {
    ammonia::clean_text(s)
}

/// Build the complete page for a message.
pub fn build_page(
    html: &str,
    header: &Header,
    inline: &HashMap<String, InlinePart>,
    allow_remote: bool,
) -> String {
    let clean = sanitize(html);
    let remote = has_remote_content(&clean);
    let body = annotate_links(&inline_cids(&clean, inline));
    let notice = if remote && !allow_remote {
        r#"<div class="rapido-notice">Remote images are blocked.</div>"#
    } else {
        ""
    };
    format!(
        r#"<!doctype html>
<html>
<head>
<meta charset="utf-8">
<meta http-equiv="Content-Security-Policy" content="{csp}">
<meta name="referrer" content="no-referrer">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>{title}</title>
<base target="_blank">
<style>
  .rapido-header {{ font: 14px/1.4 system-ui, sans-serif; background: #f4f4f5; color: #18181b;
    border-bottom: 1px solid #d4d4d8; padding: 12px 16px; margin: -8px -8px 16px; }}
  .rapido-header .s {{ font-size: 18px; font-weight: 600; margin-bottom: 4px; }}
  .rapido-header .m {{ color: #52525b; }}
  .rapido-notice {{ margin-top: 6px; color: #92400e; }}
  @media (prefers-color-scheme: dark) {{
    .rapido-header {{ background: #27272a; color: #f4f4f5; border-color: #3f3f46; }}
    .rapido-header .m {{ color: #a1a1aa; }}
    .rapido-notice {{ color: #fbbf24; }}
  }}
</style>
</head>
<body>
<div class="rapido-header">
  <div class="s">{subject}</div>
  <div class="m">From: {from}</div>
  <div class="m">To: {to}</div>
  <div class="m">{date}</div>
  {notice}
</div>
{body}
</body>
</html>
"#,
        csp = csp(allow_remote),
        title = esc(&header.subject),
        subject = esc(&header.subject),
        from = esc(&header.from),
        to = esc(&header.to),
        date = esc(&header.date),
    )
}

/// The private directory for generated pages, created 0700.
pub fn view_dir() -> Result<PathBuf> {
    let base = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let dir = base.join("rapido");
    create_private_dir(&dir)?;
    Ok(dir)
}

fn create_private_dir(dir: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)
            .with_context(|| format!("creating {}", dir.display()))?;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    #[cfg(not(unix))]
    std::fs::create_dir_all(dir)?;
    Ok(())
}

/// Write a page to a new 0600 file in `dir` and return its path.
pub fn write_page_in(dir: &Path, page: &str) -> Result<PathBuf> {
    let mut f = tempfile::Builder::new()
        .prefix("view-")
        .suffix(".html")
        .rand_bytes(16)
        .tempfile_in(dir)?;
    f.write_all(page.as_bytes())?;
    let (_, path) = f.keep()?;
    Ok(path)
}

/// Delete generated pages (at startup and on exit).
pub fn cleanup_in(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let name = e.file_name();
        let name = name.to_string_lossy();
        if name.starts_with("view-") && name.ends_with(".html") {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

pub fn cleanup() {
    if let Ok(dir) = view_dir() {
        cleanup_in(&dir);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_active_content() {
        let dirty = r#"<p onclick="evil()">Hi</p>
            <script>alert(1)</script>
            <a href="javascript:alert(1)">x</a>
            <iframe src="https://evil"></iframe>
            <form action="https://evil"><input name=p></form>
            <object data="x"></object>
            <meta http-equiv="refresh" content="0;url=https://evil">
            <link rel="stylesheet" href="https://evil/s.css">
            <base href="https://evil/">
            <img src="x" onerror="evil()">
            <svg><script>alert(1)</script></svg>
            <!-- comment -->"#;
        let clean = sanitize(dirty).to_lowercase();
        for bad in [
            "<script",
            "onclick",
            "onerror",
            "javascript:",
            "<iframe",
            "<form",
            "<input",
            "<object",
            "<meta",
            "<link",
            "<base",
            "<svg",
            "<!--",
            "alert(",
        ] {
            assert!(!clean.contains(bad), "{bad} survived: {clean}");
        }
        assert!(clean.contains("<p>hi</p>"));
    }

    #[test]
    fn keeps_styling_and_links() {
        let clean = sanitize(
            r##"<style>p{color:red}</style><table width="100%" bgcolor="#fff"><tr><td style="padding:4px">x</td></tr></table><a href="https://example.com">l</a>"##,
        );
        assert!(clean.contains("<style>p{color:red}</style>"), "{clean}");
        assert!(clean.contains(r##"bgcolor="#fff""##));
        assert!(clean.contains(r#"style="padding:4px""#));
        assert!(clean.contains(r#"rel="noopener noreferrer""#));
    }

    #[test]
    fn detects_remote_resources() {
        assert!(has_remote_content(&sanitize(
            r#"<img src="https://t.example/p.gif">"#
        )));
        assert!(has_remote_content(&sanitize(
            r#"<img src="//t.example/p.gif">"#
        )));
        assert!(has_remote_content(&sanitize(
            r#"<div style="background:url('http://t.example/a.png')">x</div>"#
        )));
        assert!(has_remote_content(&sanitize(
            r#"<style>@import "https://x/s.css";</style>"#
        )));
        assert!(!has_remote_content(&sanitize(
            r#"<img src="cid:logo@x"><a href="https://ok">link</a>"#
        )));
    }

    #[test]
    fn csp_depends_on_consent() {
        let blocked = csp(false);
        assert!(blocked.contains("script-src 'none'"));
        assert!(blocked.contains("img-src data:;"));
        assert!(!blocked.contains("https:"));
        let allowed = csp(true);
        assert!(allowed.contains("img-src data: https: http:"));
        assert!(allowed.contains("script-src 'none'"));
    }

    #[test]
    fn page_has_csp_header_escaping_and_notice() {
        let header = Header {
            from: "Eve <eve@example.com>".into(),
            subject: "<script>alert(1)</script>".into(),
            ..Default::default()
        };
        let page = build_page(
            r#"<img src="https://t/p.gif">"#,
            &header,
            &HashMap::new(),
            false,
        );
        assert!(page.contains("Content-Security-Policy"));
        assert!(page.contains("Remote images are blocked."));
        assert!(!page.contains("<script>alert(1)"));
        assert!(
            page.contains("&lt;eve@example.com&gt;"),
            "header is escaped"
        );
        let page = build_page(
            r#"<img src="https://t/p.gif">"#,
            &header,
            &HashMap::new(),
            true,
        );
        assert!(!page.contains("Remote images are blocked."));
    }

    #[test]
    fn cid_images_are_inlined() {
        let parts = HashMap::from([
            (
                "logo@x".to_string(),
                InlinePart {
                    mime: "image/png".into(),
                    bytes: vec![1, 2, 3],
                },
            ),
            (
                "svg@x".to_string(),
                InlinePart {
                    mime: "image/svg+xml".into(),
                    bytes: b"<svg/>".to_vec(),
                },
            ),
        ]);
        let page = build_page(
            r#"<img src="cid:logo@x"><img src="cid:missing"><img src="cid:svg@x">"#,
            &Header::default(),
            &parts,
            false,
        );
        assert!(
            page.contains(r#"src="data:image/png;base64,AQID""#),
            "{page}"
        );
        assert!(!page.contains("cid:"));
        assert!(!page.contains("image/svg"), "svg is never inlined");
    }

    #[test]
    fn links_get_punycode_titles() {
        let page = build_page(
            "<a href=\"https://p\u{0430}ypal.com/x?a=1&b=2\">PayPal</a>",
            &Header::default(),
            &HashMap::new(),
            false,
        );
        assert!(
            page.contains("title=\"https:&#47;&#47;xn--pypal-4ve.com"),
            "{page}"
        );
        assert!(
            page.contains("href=\"https:&#47;&#47;xn--pypal-4ve.com"),
            "{page}"
        );
        assert!(
            !page.contains("p\u{0430}ypal"),
            "unicode host gone from the page"
        );
        assert!(page.contains("(internationalized domain)"));
    }

    #[test]
    fn percent_decoding() {
        assert_eq!(percent_decode("a%40b"), "a@b");
        assert_eq!(percent_decode("bad%4"), "bad%4");
        assert_eq!(percent_decode("%"), "%");
    }

    #[cfg(unix)]
    #[test]
    fn writes_private_files_and_cleans_up() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("rapido");
        create_private_dir(&dir).unwrap();
        assert_eq!(
            std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777,
            0o700
        );
        let path = write_page_in(&dir, "<p>x</p>").unwrap();
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert!(
            path.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("view-")
        );
        std::fs::write(dir.join("other.txt"), "keep").unwrap();
        cleanup_in(&dir);
        assert!(!path.exists());
        assert!(dir.join("other.txt").exists());
    }
}
