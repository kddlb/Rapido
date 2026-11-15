use rusqlite::Connection;

/// Ordered migrations; `PRAGMA user_version` records how many have run.
const MIGRATIONS: &[&str] = &[
    r#"
CREATE TABLE mailbox (
    id             TEXT PRIMARY KEY,
    name           TEXT NOT NULL,
    parent_id      TEXT,
    role           TEXT,
    sort_order     INTEGER NOT NULL DEFAULT 0,
    total_emails   INTEGER NOT NULL DEFAULT 0,
    unread_emails  INTEGER NOT NULL DEFAULT 0,
    total_threads  INTEGER NOT NULL DEFAULT 0,
    unread_threads INTEGER NOT NULL DEFAULT 0,
    rights         TEXT,
    is_subscribed  INTEGER NOT NULL DEFAULT 1
);

CREATE TABLE email (
    id              TEXT PRIMARY KEY,
    thread_id       TEXT NOT NULL,
    blob_id         TEXT,
    received_at     INTEGER NOT NULL,
    sent_at         TEXT,
    subject         TEXT NOT NULL DEFAULT '',
    from_json       TEXT NOT NULL DEFAULT '[]',
    to_json         TEXT NOT NULL DEFAULT '[]',
    cc_json         TEXT NOT NULL DEFAULT '[]',
    reply_to_json   TEXT NOT NULL DEFAULT '[]',
    preview         TEXT NOT NULL DEFAULT '',
    size            INTEGER NOT NULL DEFAULT 0,
    has_attachment  INTEGER NOT NULL DEFAULT 0,
    message_id_json TEXT NOT NULL DEFAULT '[]',
    in_reply_to_json TEXT NOT NULL DEFAULT '[]',
    references_json TEXT NOT NULL DEFAULT '[]'
);
CREATE INDEX email_thread ON email(thread_id, received_at);

CREATE TABLE email_mailbox (
    email_id   TEXT NOT NULL REFERENCES email(id) ON DELETE CASCADE,
    mailbox_id TEXT NOT NULL,
    PRIMARY KEY (email_id, mailbox_id)
);
CREATE INDEX email_mailbox_by_mailbox ON email_mailbox(mailbox_id);

CREATE TABLE email_keyword (
    email_id TEXT NOT NULL REFERENCES email(id) ON DELETE CASCADE,
    keyword  TEXT NOT NULL,
    PRIMARY KEY (email_id, keyword)
);

CREATE TABLE email_body (
    email_id TEXT PRIMARY KEY REFERENCES email(id) ON DELETE CASCADE,
    json     TEXT NOT NULL
);

CREATE TABLE thread (
    id        TEXT PRIMARY KEY,
    email_ids TEXT NOT NULL
);

CREATE TABLE state (
    type  TEXT PRIMARY KEY,
    state TEXT NOT NULL
);

CREATE TABLE query_cache (
    mailbox_id  TEXT PRIMARY KEY,
    query_state TEXT NOT NULL,
    ids         TEXT NOT NULL,
    total       INTEGER
);

CREATE TABLE pending_op (
    id       INTEGER PRIMARY KEY AUTOINCREMENT,
    email_id TEXT NOT NULL,
    patch    TEXT NOT NULL
);
"#,
    // 2: senders whose remote images load without asking (lowercased address).
    r#"
CREATE TABLE remote_image_sender (
    address TEXT PRIMARY KEY
);
"#,
];

pub fn migrate(conn: &mut Connection) -> rusqlite::Result<()> {
    conn.pragma_update(None, "foreign_keys", true)?;
    let done: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
    for (i, sql) in MIGRATIONS.iter().enumerate().skip(done as usize) {
        let tx = conn.transaction()?;
        tx.execute_batch(sql)?;
        tx.pragma_update(None, "user_version", (i + 1) as i64)?;
        tx.commit()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrations_are_idempotent() {
        let mut c = Connection::open_in_memory().unwrap();
        migrate(&mut c).unwrap();
        migrate(&mut c).unwrap();
        let v: i64 = c
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .unwrap();
        assert_eq!(v as usize, MIGRATIONS.len());
    }
}
