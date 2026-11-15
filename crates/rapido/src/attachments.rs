//! Saving attachments to disk.

use std::path::PathBuf;

use jmap_proto::mail::EmailBodyPart;

/// Parts worth listing as attachments: everything in `attachments` except
/// inline images that the HTML already references by Content-ID.
pub fn listed(parts: &[EmailBodyPart]) -> Vec<EmailBodyPart> {
    parts
        .iter()
        .filter(|p| p.blob_id.is_some())
        .filter(|p| {
            !(p.disposition.as_deref() == Some("inline") && p.cid.is_some() && p.name.is_none())
        })
        .cloned()
        .collect()
}

/// A safe file name for `part`: no directories, control characters or leading
/// dots, never empty.
pub fn file_name(part: &EmailBodyPart) -> String {
    let raw = part.name.as_deref().unwrap_or("");
    let base = raw.rsplit(['/', '\\']).next().unwrap_or("");
    let cleaned: String = base
        .chars()
        .map(|c| {
            if c.is_control() || matches!(c, ':' | '*' | '?' | '"' | '<' | '>' | '|') {
                '_'
            } else {
                c
            }
        })
        .collect();
    let cleaned = cleaned.trim().trim_start_matches('.').trim().to_owned();
    if cleaned.is_empty() {
        let ext = part
            .content_type
            .as_deref()
            .and_then(|t| t.split('/').nth(1))
            .filter(|e| e.chars().all(|c| c.is_ascii_alphanumeric()))
            .unwrap_or("bin");
        format!("attachment.{ext}")
    } else {
        cleaned.chars().take(200).collect()
    }
}

/// Human-readable size.
pub fn size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut v = bytes as f64;
    let mut u = 0;
    while v >= 1024. && u < UNITS.len() - 1 {
        v /= 1024.;
        u += 1;
    }
    if u == 0 {
        format!("{bytes} B")
    } else {
        format!("{v:.1} {}", UNITS[u])
    }
}

/// Where the save dialog starts.
pub fn default_dir() -> PathBuf {
    directories::UserDirs::new()
        .and_then(|d| {
            d.download_dir()
                .map(PathBuf::from)
                .or_else(|| Some(d.home_dir().to_path_buf()))
        })
        .unwrap_or_else(std::env::temp_dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn part(name: Option<&str>, ty: &str) -> EmailBodyPart {
        EmailBodyPart {
            name: name.map(Into::into),
            content_type: Some(ty.into()),
            blob_id: Some("B".into()),
            ..Default::default()
        }
    }

    #[test]
    fn names_are_sanitized() {
        assert_eq!(
            file_name(&part(Some("../../etc/passwd"), "text/plain")),
            "passwd"
        );
        assert_eq!(file_name(&part(Some("C:\\evil\\x.exe"), "x/y")), "x.exe");
        assert_eq!(file_name(&part(Some(".bashrc"), "text/plain")), "bashrc");
        assert_eq!(
            file_name(&part(Some("a\u{0}b?.pdf"), "application/pdf")),
            "a_b_.pdf"
        );
        assert_eq!(file_name(&part(None, "application/pdf")), "attachment.pdf");
        assert_eq!(file_name(&part(Some("  "), "weird/x+y")), "attachment.bin");
    }

    #[test]
    fn inline_images_without_names_are_hidden() {
        let mut inline = part(None, "image/png");
        inline.disposition = Some("inline".into());
        inline.cid = Some("<x>".into());
        let doc = part(Some("doc.pdf"), "application/pdf");
        assert_eq!(listed(&[inline, doc.clone()]), vec![doc]);
    }

    #[test]
    fn sizes() {
        assert_eq!(size(512), "512 B");
        assert_eq!(size(2048), "2.0 KB");
        assert_eq!(size(5 * 1024 * 1024), "5.0 MB");
    }
}
