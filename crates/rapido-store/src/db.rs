use std::collections::HashSet;
use std::path::Path;
use std::sync::Mutex;

use chrono::{DateTime, Utc};
use jmap_proto::mail::{Email, EmailAddress, Mailbox, MailboxRights, MailboxRole, Thread, keyword};
use jmap_proto::{Id, PatchObject};
use rusqlite::{Connection, OptionalExtension, Row, Transaction, params};
use serde_json::Value;

use crate::model::{EmailBody, EmailMeta, MailboxRow, QueryCache, ThreadRow};
use crate::{Error, Result, schema};

/// The local cache. All methods are synchronous and quick; call them from any thread.
#[derive(Debug)]
pub struct Store {
    conn: Mutex<Connection>,
}

/// Keywords and mailbox membership of one email, for optimistic rollback.
#[derive(Debug, Clone, PartialEq)]
pub struct FlagSnapshot {
    pub email_id: Id,
    pub keywords: Vec<String>,
    pub mailbox_ids: Vec<Id>,
}

impl Store {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let mut conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        schema::migrate(&mut conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    pub fn open_in_memory() -> Result<Self> {
        let mut conn = Connection::open_in_memory()?;
        schema::migrate(&mut conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    fn read<T>(&self, f: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
        let conn = self.conn.lock().expect("store lock poisoned");
        f(&conn)
    }

    fn write<T>(&self, f: impl FnOnce(&Transaction) -> Result<T>) -> Result<T> {
        let mut conn = self.conn.lock().expect("store lock poisoned");
        let tx = conn.transaction()?;
        let out = f(&tx)?;
        tx.commit()?;
        Ok(out)
    }

    // ----- state tokens ---------------------------------------------------

    pub fn state(&self, ty: &str) -> Result<Option<String>> {
        self.read(|c| {
            Ok(
                c.query_row("SELECT state FROM state WHERE type = ?1", [ty], |r| {
                    r.get(0)
                })
                .optional()?,
            )
        })
    }

    pub fn set_state(&self, ty: &str, state: &str) -> Result<()> {
        self.write(|tx| set_state(tx, ty, state))
    }

    // ----- mailboxes ------------------------------------------------------

    /// Replace every mailbox (full sync) and record the state.
    pub fn replace_mailboxes(&self, list: &[Mailbox], state: &str) -> Result<()> {
        self.write(|tx| {
            tx.execute("DELETE FROM mailbox", [])?;
            for m in list {
                upsert_mailbox(tx, m)?;
            }
            set_state(tx, "Mailbox", state)
        })
    }

    /// Apply a `Mailbox/changes` round.
    pub fn apply_mailbox_changes(
        &self,
        upserts: &[Mailbox],
        destroyed: &[Id],
        state: &str,
    ) -> Result<()> {
        self.write(|tx| {
            for m in upserts {
                upsert_mailbox(tx, m)?;
            }
            for id in destroyed {
                tx.execute("DELETE FROM mailbox WHERE id = ?1", [id.as_str()])?;
            }
            set_state(tx, "Mailbox", state)
        })
    }

    pub fn mailboxes(&self) -> Result<Vec<MailboxRow>> {
        self.read(|c| {
            let mut st = c.prepare(
                "SELECT id, name, parent_id, role, sort_order, total_emails, unread_emails,
                        total_threads, unread_threads, rights, is_subscribed FROM mailbox",
            )?;
            let rows = st.query_map([], |r| {
                Ok(MailboxRow {
                    id: Id(r.get(0)?),
                    name: r.get(1)?,
                    parent_id: r.get::<_, Option<String>>(2)?.map(Id),
                    role: r
                        .get::<_, Option<String>>(3)?
                        .map(|s| MailboxRole::from(s.as_str())),
                    sort_order: r.get(4)?,
                    total_emails: r.get::<_, i64>(5)? as u64,
                    unread_emails: r.get::<_, i64>(6)? as u64,
                    total_threads: r.get::<_, i64>(7)? as u64,
                    unread_threads: r.get::<_, i64>(8)? as u64,
                    rights: r
                        .get::<_, Option<String>>(9)?
                        .and_then(|s| serde_json::from_str::<MailboxRights>(&s).ok()),
                    is_subscribed: r.get(10)?,
                    depth: 0,
                })
            })?;
            Ok(rows.collect::<rusqlite::Result<_>>()?)
        })
    }

    pub fn mailbox_by_role(&self, role: &MailboxRole) -> Result<Option<Id>> {
        self.read(|c| {
            Ok(c.query_row(
                "SELECT id FROM mailbox WHERE role = ?1 ORDER BY sort_order LIMIT 1",
                [role.as_str()],
                |r| r.get::<_, String>(0),
            )
            .optional()?
            .map(Id))
        })
    }

    // ----- emails ---------------------------------------------------------

    /// Insert or replace emails fetched with full metadata properties.
    pub fn upsert_emails(&self, emails: &[Email]) -> Result<()> {
        self.write(|tx| {
            for e in emails {
                upsert_email(tx, e)?;
            }
            Ok(())
        })
    }

    /// Update keywords/mailboxIds from emails fetched with only those properties.
    /// Unknown emails are ignored.
    pub fn update_email_flags(&self, emails: &[Email]) -> Result<()> {
        self.write(|tx| {
            for e in emails {
                if !email_exists(tx, &e.id)? {
                    continue;
                }
                if let Some(kw) = &e.keywords {
                    set_keywords(
                        tx,
                        &e.id,
                        kw.iter().filter(|(_, v)| **v).map(|(k, _)| k.as_str()),
                    )?;
                }
                if let Some(mb) = &e.mailbox_ids {
                    set_mailbox_ids(tx, &e.id, mb.iter().filter(|(_, v)| **v).map(|(k, _)| k))?;
                }
            }
            Ok(())
        })
    }

    /// Delete emails; returns the thread ids they belonged to.
    pub fn delete_emails(&self, ids: &[Id]) -> Result<Vec<Id>> {
        self.write(|tx| {
            let mut threads = HashSet::new();
            for id in ids {
                if let Some(t) = tx
                    .query_row(
                        "SELECT thread_id FROM email WHERE id = ?1",
                        [id.as_str()],
                        |r| r.get::<_, String>(0),
                    )
                    .optional()?
                {
                    threads.insert(Id(t));
                }
                tx.execute("DELETE FROM email WHERE id = ?1", [id.as_str()])?;
            }
            Ok(threads.into_iter().collect())
        })
    }

    /// Drop all cached email data (for a full resync). Returns the mailboxes that had cached lists.
    pub fn clear_emails(&self) -> Result<Vec<Id>> {
        self.write(|tx| {
            let mut st = tx.prepare("SELECT mailbox_id FROM query_cache")?;
            let mailboxes = st
                .query_map([], |r| r.get::<_, String>(0).map(Id))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            drop(st);
            tx.execute_batch(
                "DELETE FROM email; DELETE FROM thread; DELETE FROM query_cache;
                 DELETE FROM state WHERE type IN ('Email', 'Thread');",
            )?;
            Ok(mailboxes)
        })
    }

    pub fn email(&self, id: &Id) -> Result<Option<EmailMeta>> {
        self.read(|c| {
            let meta = c
                .query_row(
                    &format!("SELECT {EMAIL_COLS} FROM email WHERE id = ?1"),
                    [id.as_str()],
                    email_meta_row,
                )
                .optional()?;
            meta.map(|m| fill_flags(c, m)).transpose()
        })
    }

    /// Ids among `ids` that are not stored.
    pub fn missing_emails(&self, ids: &[Id]) -> Result<Vec<Id>> {
        self.read(|c| {
            let mut out = Vec::new();
            for id in ids {
                if !email_exists(c, id)? {
                    out.push(id.clone());
                }
            }
            Ok(out)
        })
    }

    /// Thread ids of the given (stored) emails.
    pub fn thread_ids_of(&self, ids: &[Id]) -> Result<Vec<Id>> {
        self.read(|c| {
            let mut set = HashSet::new();
            for id in ids {
                if let Some(t) = c
                    .query_row(
                        "SELECT thread_id FROM email WHERE id = ?1",
                        [id.as_str()],
                        |r| r.get::<_, String>(0),
                    )
                    .optional()?
                {
                    set.insert(Id(t));
                }
            }
            Ok(set.into_iter().collect())
        })
    }

    /// All stored emails of a thread, oldest first.
    pub fn thread_emails(&self, thread_id: &Id) -> Result<Vec<EmailMeta>> {
        self.read(|c| {
            let mut st = c.prepare(&format!(
                "SELECT {EMAIL_COLS} FROM email WHERE thread_id = ?1 ORDER BY received_at, id"
            ))?;
            let metas = st
                .query_map([thread_id.as_str()], email_meta_row)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            metas.into_iter().map(|m| fill_flags(c, m)).collect()
        })
    }

    pub fn upsert_threads(&self, threads: &[Thread]) -> Result<()> {
        self.write(|tx| {
            for t in threads {
                tx.execute(
                    "INSERT INTO thread (id, email_ids) VALUES (?1, ?2)
                     ON CONFLICT(id) DO UPDATE SET email_ids = excluded.email_ids",
                    params![t.id.as_str(), serde_json::to_string(&t.email_ids)?],
                )?;
            }
            Ok(())
        })
    }

    pub fn thread_email_ids(&self, thread_id: &Id) -> Result<Option<Vec<Id>>> {
        self.read(|c| {
            let s: Option<String> = c
                .query_row(
                    "SELECT email_ids FROM thread WHERE id = ?1",
                    [thread_id.as_str()],
                    |r| r.get(0),
                )
                .optional()?;
            Ok(s.map(|s| serde_json::from_str(&s)).transpose()?)
        })
    }

    // ----- bodies ---------------------------------------------------------

    pub fn save_body(&self, id: &Id, body: &EmailBody) -> Result<()> {
        self.write(|tx| {
            tx.execute(
                "INSERT OR REPLACE INTO email_body (email_id, json) VALUES (?1, ?2)",
                params![id.as_str(), serde_json::to_string(body)?],
            )?;
            Ok(())
        })
    }

    pub fn body(&self, id: &Id) -> Result<Option<EmailBody>> {
        self.read(|c| {
            let s: Option<String> = c
                .query_row(
                    "SELECT json FROM email_body WHERE email_id = ?1",
                    [id.as_str()],
                    |r| r.get(0),
                )
                .optional()?;
            Ok(s.map(|s| serde_json::from_str(&s)).transpose()?)
        })
    }

    // ----- query cache ----------------------------------------------------

    pub fn query(&self, mailbox: &Id) -> Result<Option<QueryCache>> {
        self.read(|c| {
            let row: Option<(String, String, Option<i64>)> = c
                .query_row(
                    "SELECT query_state, ids, total FROM query_cache WHERE mailbox_id = ?1",
                    [mailbox.as_str()],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .optional()?;
            row.map(|(query_state, ids, total)| {
                Ok(QueryCache {
                    query_state,
                    ids: serde_json::from_str(&ids)?,
                    total: total.map(|t| t as u64),
                })
            })
            .transpose()
        })
    }

    pub fn cached_query_mailboxes(&self) -> Result<Vec<Id>> {
        self.read(|c| {
            let mut st = c.prepare("SELECT mailbox_id FROM query_cache")?;
            Ok(st
                .query_map([], |r| r.get::<_, String>(0).map(Id))?
                .collect::<rusqlite::Result<_>>()?)
        })
    }

    /// Store a query page. `position == 0` replaces the cache; otherwise the page
    /// is spliced in at `position` (truncating anything after it).
    pub fn put_query_page(
        &self,
        mailbox: &Id,
        query_state: &str,
        position: usize,
        page: &[Id],
        total: Option<u64>,
    ) -> Result<()> {
        let mut ids = if position == 0 {
            Vec::new()
        } else {
            self.query(mailbox)?.map(|q| q.ids).unwrap_or_default()
        };
        ids.truncate(position);
        ids.extend(page.iter().cloned());
        self.set_query(mailbox, query_state, &ids, total)
    }

    pub fn set_query(
        &self,
        mailbox: &Id,
        query_state: &str,
        ids: &[Id],
        total: Option<u64>,
    ) -> Result<()> {
        self.write(|tx| {
            tx.execute(
                "INSERT OR REPLACE INTO query_cache (mailbox_id, query_state, ids, total)
                 VALUES (?1, ?2, ?3, ?4)",
                params![
                    mailbox.as_str(),
                    query_state,
                    serde_json::to_string(ids)?,
                    total.map(|t| t as i64)
                ],
            )?;
            Ok(())
        })
    }

    /// Apply `queryChanges` to the cached prefix. Added items beyond the cached
    /// prefix are dropped (they will load with the next page).
    pub fn apply_query_changes(
        &self,
        mailbox: &Id,
        new_state: &str,
        removed: &[Id],
        added: &[(Id, u64)],
        total: Option<u64>,
    ) -> Result<()> {
        let Some(mut q) = self.query(mailbox)? else {
            return Err(Error::NotFound(format!("query cache for {mailbox}")));
        };
        let removed: HashSet<&Id> = removed.iter().collect();
        q.ids.retain(|id| !removed.contains(id));
        let mut added = added.to_vec();
        added.sort_by_key(|(_, i)| *i);
        let prefix_len = q.ids.len() + added.len();
        for (id, index) in added {
            let index = index as usize;
            if index <= q.ids.len() && index < prefix_len {
                q.ids.insert(index, id);
            }
        }
        self.set_query(mailbox, new_state, &q.ids, total.or(q.total))
    }

    /// Thread rows for a mailbox, in cached query order. Rows whose email has left
    /// the mailbox (e.g. optimistically moved) are skipped.
    pub fn thread_list(&self, mailbox: &Id) -> Result<Vec<ThreadRow>> {
        let Some(q) = self.query(mailbox)? else {
            return Ok(Vec::new());
        };
        self.thread_rows(&q.ids, Some(mailbox))
    }

    /// Thread rows for representative email ids, in order. With `mailbox`,
    /// emails no longer in it are skipped; unknown emails are always skipped.
    pub fn thread_rows(&self, ids: &[Id], mailbox: Option<&Id>) -> Result<Vec<ThreadRow>> {
        self.read(|c| {
            let mut rows = Vec::with_capacity(ids.len());
            let mut in_mailbox = c.prepare(
                "SELECT 1 FROM email_mailbox WHERE email_id = ?1 AND mailbox_id = ?2",
            )?;
            let mut rep = c.prepare(
                "SELECT thread_id, subject, preview, received_at, has_attachment
                 FROM email WHERE id = ?1",
            )?;
            let mut members = c.prepare(
                "SELECT e.from_json,
                        EXISTS(SELECT 1 FROM email_keyword k WHERE k.email_id = e.id AND k.keyword = ?2),
                        EXISTS(SELECT 1 FROM email_keyword k WHERE k.email_id = e.id AND k.keyword = ?3),
                        e.has_attachment
                 FROM email e WHERE e.thread_id = ?1 ORDER BY e.received_at, e.id",
            )?;
            for id in ids {
                if let Some(mb) = mailbox
                    && !in_mailbox.exists(params![id.as_str(), mb.as_str()])?
                {
                    continue;
                }
                let Some((thread_id, subject, preview, received, has_att)) = rep
                    .query_row([id.as_str()], |r| {
                        Ok((
                            r.get::<_, String>(0)?,
                            r.get::<_, String>(1)?,
                            r.get::<_, String>(2)?,
                            r.get::<_, i64>(3)?,
                            r.get::<_, bool>(4)?,
                        ))
                    })
                    .optional()?
                else {
                    continue;
                };
                let mut senders: Vec<EmailAddress> = Vec::new();
                let (mut count, mut unread, mut flagged, mut any_att) = (0, false, false, has_att);
                let mut m = members.query(params![thread_id, keyword::SEEN, keyword::FLAGGED])?;
                while let Some(r) = m.next()? {
                    count += 1;
                    let from: Vec<EmailAddress> =
                        serde_json::from_str(&r.get::<_, String>(0)?).unwrap_or_default();
                    for a in from {
                        if senders.len() < 3 && !senders.iter().any(|s| s.email.eq_ignore_ascii_case(&a.email)) {
                            senders.push(a);
                        }
                    }
                    unread |= !r.get::<_, bool>(1)?;
                    flagged |= r.get::<_, bool>(2)?;
                    any_att |= r.get::<_, bool>(3)?;
                }
                rows.push(ThreadRow {
                    email_id: id.clone(),
                    thread_id: Id(thread_id),
                    subject,
                    senders,
                    preview,
                    received_at: from_unix(received),
                    unread,
                    flagged,
                    count: count.max(1),
                    has_attachment: any_att,
                });
            }
            Ok(rows)
        })
    }

    // ----- remote image allow list ---------------------------------------

    pub fn allow_remote_images(&self, address: &str) -> Result<()> {
        self.write(|tx| {
            tx.execute(
                "INSERT OR IGNORE INTO remote_image_sender (address) VALUES (?1)",
                [address.trim().to_lowercase()],
            )?;
            Ok(())
        })
    }

    pub fn forget_remote_images(&self, address: &str) -> Result<()> {
        self.write(|tx| {
            tx.execute(
                "DELETE FROM remote_image_sender WHERE address = ?1",
                [address.trim().to_lowercase()],
            )?;
            Ok(())
        })
    }

    pub fn remote_images_allowed(&self, address: &str) -> Result<bool> {
        self.read(|c| {
            Ok(
                c.prepare_cached("SELECT 1 FROM remote_image_sender WHERE address = ?1")?
                    .exists([address.trim().to_lowercase()])?,
            )
        })
    }

    pub fn remote_image_senders(&self) -> Result<Vec<String>> {
        self.read(|c| {
            let mut st = c.prepare("SELECT address FROM remote_image_sender ORDER BY address")?;
            Ok(st
                .query_map([], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?)
        })
    }

    // ----- optimistic triage ---------------------------------------------

    pub fn snapshot_flags(&self, ids: &[Id]) -> Result<Vec<FlagSnapshot>> {
        self.read(|c| {
            ids.iter()
                .filter_map(|id| match email_exists(c, id) {
                    Ok(true) => Some(Ok(id)),
                    Ok(false) => None,
                    Err(e) => Some(Err(e)),
                })
                .map(|id| {
                    let id = id?;
                    Ok(FlagSnapshot {
                        email_id: id.clone(),
                        keywords: keywords_of(c, id)?,
                        mailbox_ids: mailboxes_of(c, id)?,
                    })
                })
                .collect()
        })
    }

    pub fn restore_flags(&self, snaps: &[FlagSnapshot]) -> Result<()> {
        self.write(|tx| {
            for s in snaps {
                set_keywords(tx, &s.email_id, s.keywords.iter().map(String::as_str))?;
                set_mailbox_ids(tx, &s.email_id, s.mailbox_ids.iter())?;
            }
            Ok(())
        })
    }

    /// Apply `Email/set` patches locally, interpreting the same patch objects
    /// that are sent to the server.
    pub fn apply_patches(&self, patches: &[(Id, PatchObject)]) -> Result<()> {
        self.write(|tx| {
            for (id, patch) in patches {
                apply_patch(tx, id, patch)?;
            }
            Ok(())
        })
    }

    pub fn queue_ops(&self, patches: &[(Id, PatchObject)]) -> Result<()> {
        self.write(|tx| {
            for (id, patch) in patches {
                tx.execute(
                    "INSERT INTO pending_op (email_id, patch) VALUES (?1, ?2)",
                    params![id.as_str(), serde_json::to_string(patch)?],
                )?;
            }
            Ok(())
        })
    }

    /// Queued ops in insertion order: `(op id, email id, patch)`.
    pub fn pending_ops(&self) -> Result<Vec<(i64, Id, PatchObject)>> {
        self.read(|c| {
            let mut st = c.prepare("SELECT id, email_id, patch FROM pending_op ORDER BY id")?;
            let rows = st
                .query_map([], |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            rows.into_iter()
                .map(|(op, id, p)| Ok((op, Id(id), serde_json::from_str(&p)?)))
                .collect()
        })
    }

    pub fn remove_op(&self, op: i64) -> Result<()> {
        self.write(|tx| {
            tx.execute("DELETE FROM pending_op WHERE id = ?1", [op])?;
            Ok(())
        })
    }
}

// ----- helpers ------------------------------------------------------------

const EMAIL_COLS: &str =
    "id, thread_id, blob_id, received_at, sent_at, subject, from_json, to_json,
     cc_json, reply_to_json, preview, size, has_attachment";

fn email_meta_row(r: &Row) -> rusqlite::Result<EmailMeta> {
    let addrs = |i: usize| -> rusqlite::Result<Vec<EmailAddress>> {
        Ok(serde_json::from_str(&r.get::<_, String>(i)?).unwrap_or_default())
    };
    Ok(EmailMeta {
        id: Id(r.get(0)?),
        thread_id: Id(r.get(1)?),
        blob_id: r.get::<_, Option<String>>(2)?.map(Id),
        mailbox_ids: Vec::new(),
        keywords: Vec::new(),
        received_at: from_unix(r.get(3)?),
        sent_at: r
            .get::<_, Option<String>>(4)?
            .and_then(|s| DateTime::parse_from_rfc3339(&s).ok()),
        subject: r.get(5)?,
        from: addrs(6)?,
        to: addrs(7)?,
        cc: addrs(8)?,
        reply_to: addrs(9)?,
        preview: r.get(10)?,
        size: r.get::<_, i64>(11)? as u64,
        has_attachment: r.get(12)?,
    })
}

fn fill_flags(c: &Connection, mut m: EmailMeta) -> Result<EmailMeta> {
    m.keywords = keywords_of(c, &m.id)?;
    m.mailbox_ids = mailboxes_of(c, &m.id)?;
    Ok(m)
}

fn from_unix(secs: i64) -> DateTime<Utc> {
    DateTime::from_timestamp(secs, 0).unwrap_or_default()
}

fn set_state(tx: &Transaction, ty: &str, state: &str) -> Result<()> {
    tx.execute(
        "INSERT OR REPLACE INTO state (type, state) VALUES (?1, ?2)",
        params![ty, state],
    )?;
    Ok(())
}

fn upsert_mailbox(tx: &Transaction, m: &Mailbox) -> Result<()> {
    tx.execute(
        "INSERT OR REPLACE INTO mailbox (id, name, parent_id, role, sort_order, total_emails,
             unread_emails, total_threads, unread_threads, rights, is_subscribed)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        params![
            m.id.as_str(),
            m.name.clone().unwrap_or_default(),
            m.parent_id.as_ref().map(Id::as_str),
            m.role.as_ref().map(MailboxRole::as_str),
            m.sort_order.unwrap_or(0),
            m.total_emails.unwrap_or(0) as i64,
            m.unread_emails.unwrap_or(0) as i64,
            m.total_threads.unwrap_or(0) as i64,
            m.unread_threads.unwrap_or(0) as i64,
            m.my_rights.map(|r| serde_json::to_string(&r)).transpose()?,
            m.is_subscribed.unwrap_or(true),
        ],
    )?;
    Ok(())
}

fn upsert_email(tx: &Transaction, e: &Email) -> Result<()> {
    let json = |v: &Option<Vec<EmailAddress>>| serde_json::to_string(v.as_deref().unwrap_or(&[]));
    let strs = |v: &Option<Vec<String>>| serde_json::to_string(v.as_deref().unwrap_or(&[]));
    let thread = e
        .thread_id
        .as_ref()
        .ok_or_else(|| Error::NotFound(format!("threadId for email {}", e.id)))?;
    tx.execute(
        "INSERT INTO email (id, thread_id, blob_id, received_at, sent_at, subject, from_json,
             to_json, cc_json, reply_to_json, preview, size, has_attachment, message_id_json,
             in_reply_to_json, references_json)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)
         ON CONFLICT(id) DO UPDATE SET
             thread_id = excluded.thread_id, blob_id = excluded.blob_id,
             received_at = excluded.received_at, sent_at = excluded.sent_at,
             subject = excluded.subject, from_json = excluded.from_json, to_json = excluded.to_json,
             cc_json = excluded.cc_json, reply_to_json = excluded.reply_to_json,
             preview = excluded.preview, size = excluded.size,
             has_attachment = excluded.has_attachment, message_id_json = excluded.message_id_json,
             in_reply_to_json = excluded.in_reply_to_json, references_json = excluded.references_json",
        params![
            e.id.as_str(),
            thread.as_str(),
            e.blob_id.as_ref().map(Id::as_str),
            e.received_at.map(|d| d.timestamp()).unwrap_or(0),
            e.sent_at.map(|d| d.to_rfc3339()),
            e.subject.clone().unwrap_or_default(),
            json(&e.from)?,
            json(&e.to)?,
            json(&e.cc)?,
            json(&e.reply_to)?,
            e.preview.clone().unwrap_or_default(),
            e.size.unwrap_or(0) as i64,
            e.has_attachment.unwrap_or(false),
            strs(&e.message_id)?,
            strs(&e.in_reply_to)?,
            strs(&e.references)?,
        ],
    )?;
    if let Some(kw) = &e.keywords {
        set_keywords(
            tx,
            &e.id,
            kw.iter().filter(|(_, v)| **v).map(|(k, _)| k.as_str()),
        )?;
    }
    if let Some(mb) = &e.mailbox_ids {
        set_mailbox_ids(tx, &e.id, mb.iter().filter(|(_, v)| **v).map(|(k, _)| k))?;
    }
    Ok(())
}

fn email_exists(c: &Connection, id: &Id) -> Result<bool> {
    Ok(c.prepare_cached("SELECT 1 FROM email WHERE id = ?1")?
        .exists([id.as_str()])?)
}

fn keywords_of(c: &Connection, id: &Id) -> Result<Vec<String>> {
    let mut st =
        c.prepare_cached("SELECT keyword FROM email_keyword WHERE email_id = ?1 ORDER BY keyword")?;
    Ok(st
        .query_map([id.as_str()], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?)
}

fn mailboxes_of(c: &Connection, id: &Id) -> Result<Vec<Id>> {
    let mut st = c.prepare_cached(
        "SELECT mailbox_id FROM email_mailbox WHERE email_id = ?1 ORDER BY mailbox_id",
    )?;
    Ok(st
        .query_map([id.as_str()], |r| r.get::<_, String>(0).map(Id))?
        .collect::<rusqlite::Result<_>>()?)
}

fn set_keywords<'a>(tx: &Transaction, id: &Id, kws: impl Iterator<Item = &'a str>) -> Result<()> {
    tx.execute(
        "DELETE FROM email_keyword WHERE email_id = ?1",
        [id.as_str()],
    )?;
    for k in kws {
        tx.execute(
            "INSERT OR IGNORE INTO email_keyword (email_id, keyword) VALUES (?1, ?2)",
            params![id.as_str(), k.to_lowercase()],
        )?;
    }
    Ok(())
}

fn set_mailbox_ids<'a>(tx: &Transaction, id: &Id, mbs: impl Iterator<Item = &'a Id>) -> Result<()> {
    tx.execute(
        "DELETE FROM email_mailbox WHERE email_id = ?1",
        [id.as_str()],
    )?;
    for m in mbs {
        tx.execute(
            "INSERT OR IGNORE INTO email_mailbox (email_id, mailbox_id) VALUES (?1, ?2)",
            params![id.as_str(), m.as_str()],
        )?;
    }
    Ok(())
}

fn unescape_pointer(s: &str) -> String {
    s.replace("~1", "/").replace("~0", "~")
}

fn apply_patch(tx: &Transaction, id: &Id, patch: &PatchObject) -> Result<()> {
    for (path, value) in patch {
        let on = matches!(value, Value::Bool(true));
        if path == "mailboxIds" {
            let ids: Vec<Id> = value
                .as_object()
                .map(|m| {
                    m.iter()
                        .filter(|(_, v)| v.as_bool() == Some(true))
                        .map(|(k, _)| Id(k.clone()))
                        .collect()
                })
                .unwrap_or_default();
            set_mailbox_ids(tx, id, ids.iter())?;
        } else if path == "keywords" {
            let kws: Vec<String> = value
                .as_object()
                .map(|m| {
                    m.iter()
                        .filter(|(_, v)| v.as_bool() == Some(true))
                        .map(|(k, _)| k.clone())
                        .collect()
                })
                .unwrap_or_default();
            set_keywords(tx, id, kws.iter().map(String::as_str))?;
        } else if let Some(kw) = path.strip_prefix("keywords/") {
            let kw = unescape_pointer(kw).to_lowercase();
            if on {
                tx.execute(
                    "INSERT OR IGNORE INTO email_keyword (email_id, keyword) VALUES (?1, ?2)",
                    params![id.as_str(), kw],
                )?;
            } else {
                tx.execute(
                    "DELETE FROM email_keyword WHERE email_id = ?1 AND keyword = ?2",
                    params![id.as_str(), kw],
                )?;
            }
        } else if let Some(mb) = path.strip_prefix("mailboxIds/") {
            let mb = unescape_pointer(mb);
            if on {
                tx.execute(
                    "INSERT OR IGNORE INTO email_mailbox (email_id, mailbox_id) VALUES (?1, ?2)",
                    params![id.as_str(), mb],
                )?;
            } else {
                tx.execute(
                    "DELETE FROM email_mailbox WHERE email_id = ?1 AND mailbox_id = ?2",
                    params![id.as_str(), mb],
                )?;
            }
        } else {
            tracing::warn!("ignoring unsupported local patch path {path}");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use jmap_proto::mail::patch;

    fn email(id: &str, thread: &str, mailbox: &str, t: i64, seen: bool) -> Email {
        Email {
            id: id.into(),
            thread_id: Some(thread.into()),
            mailbox_ids: Some([(Id::from(mailbox), true)].into()),
            keywords: Some(if seen {
                [("$seen".to_string(), true)].into()
            } else {
                Default::default()
            }),
            received_at: DateTime::from_timestamp(t, 0),
            subject: Some(format!("subject {id}")),
            from: Some(vec![EmailAddress {
                name: Some(format!("Sender {id}")),
                email: format!("{id}@x"),
            }]),
            ..Default::default()
        }
    }

    #[test]
    fn thread_list_aggregates_and_skips_moved() {
        let s = Store::open_in_memory().unwrap();
        s.upsert_emails(&[
            email("m1", "t1", "inbox", 100, true),
            email("m2", "t1", "sent", 200, false),
            email("m3", "t2", "inbox", 150, true),
        ])
        .unwrap();
        s.put_query_page(
            &"inbox".into(),
            "q1",
            0,
            &["m1".into(), "m3".into()],
            Some(2),
        )
        .unwrap();
        let rows = s.thread_list(&"inbox".into()).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].count, 2);
        assert!(rows[0].unread, "m2 in the thread is unread");
        assert_eq!(rows[0].senders.len(), 2);
        assert!(!rows[1].unread);

        let mut p = PatchObject::new();
        patch::mailbox(&mut p, &"inbox".into(), false);
        patch::mailbox(&mut p, &"archive".into(), true);
        s.apply_patches(&[("m3".into(), p)]).unwrap();
        assert_eq!(s.thread_list(&"inbox".into()).unwrap().len(), 1);
        assert_eq!(
            s.email(&"m3".into()).unwrap().unwrap().mailbox_ids,
            vec![Id::from("archive")]
        );
    }

    #[test]
    fn snapshot_restore_round_trip() {
        let s = Store::open_in_memory().unwrap();
        s.upsert_emails(&[email("m1", "t1", "inbox", 1, false)])
            .unwrap();
        let snap = s.snapshot_flags(&["m1".into(), "nope".into()]).unwrap();
        assert_eq!(snap.len(), 1);
        let mut p = PatchObject::new();
        patch::keyword(&mut p, "$seen", true);
        patch::set_mailboxes(&mut p, [&Id::from("trash")]);
        s.apply_patches(&[("m1".into(), p)]).unwrap();
        let e = s.email(&"m1".into()).unwrap().unwrap();
        assert!(e.is_seen());
        assert_eq!(e.mailbox_ids, vec![Id::from("trash")]);
        s.restore_flags(&snap).unwrap();
        let e = s.email(&"m1".into()).unwrap().unwrap();
        assert!(!e.is_seen());
        assert_eq!(e.mailbox_ids, vec![Id::from("inbox")]);
    }

    #[test]
    fn query_changes_splice_into_prefix() {
        let s = Store::open_in_memory().unwrap();
        let mb: Id = "inbox".into();
        let ids: Vec<Id> = ["a", "b", "c"].map(Id::from).to_vec();
        s.set_query(&mb, "1", &ids, Some(10)).unwrap();
        s.apply_query_changes(
            &mb,
            "2",
            &["b".into()],
            &[("n".into(), 0), ("far".into(), 9)],
            Some(10),
        )
        .unwrap();
        let q = s.query(&mb).unwrap().unwrap();
        assert_eq!(q.ids, ["n", "a", "c"].map(Id::from).to_vec());
        assert_eq!(q.query_state, "2");
    }

    #[test]
    fn delete_returns_threads_and_cascades() {
        let s = Store::open_in_memory().unwrap();
        s.upsert_emails(&[email("m1", "t1", "inbox", 1, false)])
            .unwrap();
        s.save_body(&"m1".into(), &EmailBody::default()).unwrap();
        assert_eq!(
            s.delete_emails(&["m1".into()]).unwrap(),
            vec![Id::from("t1")]
        );
        assert!(s.body(&"m1".into()).unwrap().is_none());
        assert!(s.snapshot_flags(&["m1".into()]).unwrap().is_empty());
    }

    #[test]
    fn remote_image_allow_list() {
        let s = Store::open_in_memory().unwrap();
        assert!(!s.remote_images_allowed("News@Example.com").unwrap());
        s.allow_remote_images(" News@Example.com ").unwrap();
        s.allow_remote_images("news@example.com").unwrap();
        assert!(s.remote_images_allowed("NEWS@example.COM").unwrap());
        assert_eq!(s.remote_image_senders().unwrap(), ["news@example.com"]);
        s.forget_remote_images("News@example.com").unwrap();
        assert!(s.remote_image_senders().unwrap().is_empty());
    }

    #[test]
    fn pending_ops_queue() {
        let s = Store::open_in_memory().unwrap();
        let mut p = PatchObject::new();
        patch::keyword(&mut p, "$flagged", true);
        s.queue_ops(&[("m1".into(), p.clone())]).unwrap();
        let ops = s.pending_ops().unwrap();
        assert_eq!(ops.len(), 1);
        assert_eq!(ops[0].2, p);
        s.remove_op(ops[0].0).unwrap();
        assert!(s.pending_ops().unwrap().is_empty());
    }
}
