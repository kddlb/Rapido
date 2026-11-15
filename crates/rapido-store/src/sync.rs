use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use jmap_http::{JmapClient, PushEvent, chunk_ids};
use jmap_proto::mail::*;
use jmap_proto::{Changes, Comparator, Get, Id, MethodErrorType, PatchObject, Query, QueryChanges};
use tokio::sync::broadcast;

use crate::model::{EmailBody, EmailMeta, StoreEvent};
use crate::{Error, Result, Store};

/// Rows fetched per `Email/query` page.
pub const PAGE_SIZE: u64 = 50;

const METADATA: &[EmailProperty] = &[
    EmailProperty::Id,
    EmailProperty::BlobId,
    EmailProperty::ThreadId,
    EmailProperty::MailboxIds,
    EmailProperty::Keywords,
    EmailProperty::Size,
    EmailProperty::ReceivedAt,
    EmailProperty::MessageId,
    EmailProperty::InReplyTo,
    EmailProperty::References,
    EmailProperty::From,
    EmailProperty::To,
    EmailProperty::Cc,
    EmailProperty::ReplyTo,
    EmailProperty::Subject,
    EmailProperty::SentAt,
    EmailProperty::HasAttachment,
    EmailProperty::Preview,
];

/// The only mutable Email properties (RFC 8621 §4.1).
const FLAGS: &[EmailProperty] = &[
    EmailProperty::Id,
    EmailProperty::Keywords,
    EmailProperty::MailboxIds,
];

/// One page of search results: representative email ids (threads collapsed),
/// newest first, with highlighted snippets.
#[derive(Debug, Clone, PartialEq)]
pub struct SearchPage {
    pub ids: Vec<Id>,
    pub position: u64,
    pub total: Option<u64>,
    pub snippets: std::collections::HashMap<Id, SearchSnippet>,
}

/// A triage action on a set of emails.
#[derive(Debug, Clone, PartialEq)]
pub enum TriageOp {
    SetSeen(bool),
    SetFlagged(bool),
    /// Move from one mailbox to another, keeping other memberships.
    Move {
        from: Id,
        to: Id,
    },
    /// Move to the account's Archive mailbox from `from`.
    Archive {
        from: Id,
    },
    /// Move to Trash, dropping all other memberships.
    Trash,
}

/// Keeps a [`Store`] in sync with one JMAP account.
pub struct SyncEngine {
    client: Arc<JmapClient>,
    store: Arc<Store>,
    account: Id,
    events: broadcast::Sender<StoreEvent>,
}

fn is_cannot_calculate(e: &jmap_http::Error) -> bool {
    e.method_error().is_some_and(|m| {
        matches!(
            m.kind,
            MethodErrorType::CannotCalculateChanges | MethodErrorType::TooManyChanges
        )
    })
}

fn inbox_query(account: &Id, mailbox: &Id) -> Query<EmailFilterCondition, EmailSortProperty> {
    let mut q = Query::new(account.clone());
    q.filter = Some(
        EmailFilterCondition {
            in_mailbox: Some(mailbox.clone()),
            ..Default::default()
        }
        .into(),
    );
    q.sort = Some(vec![Comparator::desc(EmailSortProperty::ReceivedAt)]);
    q
}

impl SyncEngine {
    pub fn new(client: Arc<JmapClient>, store: Arc<Store>, account: Id) -> Self {
        let (events, _) = broadcast::channel(256);
        Self {
            client,
            store,
            account,
            events,
        }
    }

    pub fn store(&self) -> &Arc<Store> {
        &self.store
    }

    pub fn client(&self) -> &Arc<JmapClient> {
        &self.client
    }

    pub fn account(&self) -> &Id {
        &self.account
    }

    pub fn subscribe(&self) -> broadcast::Receiver<StoreEvent> {
        self.events.subscribe()
    }

    fn emit(&self, ev: StoreEvent) {
        let _ = self.events.send(ev);
    }

    fn get_limit(&self) -> u64 {
        self.client.session().core().max_objects_in_get
    }

    // ----- mailboxes ------------------------------------------------------

    pub async fn sync_mailboxes(&self) -> Result<()> {
        let Some(mut since) = self.store.state("Mailbox")? else {
            return self.full_mailboxes().await;
        };
        loop {
            let mut b = self.client.batch();
            let ch = b.call(MailboxChanges(Changes {
                account_id: self.account.clone(),
                since_state: since.as_str().into(),
                max_changes: Some(self.get_limit()),
            }))?;
            let created = b.call(MailboxGet(
                Get::new(self.account.clone()).ids_ref(ch.result_ref("/created")),
            ))?;
            let updated = b.call(MailboxGet(
                Get::new(self.account.clone()).ids_ref(ch.result_ref("/updated")),
            ))?;
            let r = b.send().await?;
            let changes = match r.get(&ch) {
                Err(e) if is_cannot_calculate(&e) => return self.full_mailboxes().await,
                other => other?,
            };
            let mut upserts = r.get(&created)?.list;
            upserts.extend(r.get(&updated)?.list);
            let changed = !upserts.is_empty() || !changes.destroyed.is_empty();
            self.store.apply_mailbox_changes(
                &upserts,
                &changes.destroyed,
                changes.new_state.as_str(),
            )?;
            if changed {
                self.emit(StoreEvent::MailboxesChanged);
            }
            if !changes.has_more_changes {
                return Ok(());
            }
            since = changes.new_state.0;
        }
    }

    async fn full_mailboxes(&self) -> Result<()> {
        let mut b = self.client.batch();
        let g = b.call(MailboxGet(Get::new(self.account.clone())))?;
        let r = b.send().await?.get(&g)?;
        self.store.replace_mailboxes(&r.list, r.state.as_str())?;
        self.emit(StoreEvent::MailboxesChanged);
        Ok(())
    }

    // ----- message lists --------------------------------------------------

    /// Fetch one page of a mailbox's thread list, plus metadata for every email
    /// in those threads, in a single round trip.
    pub async fn load_mailbox(&self, mailbox: &Id, position: u64, limit: u64) -> Result<()> {
        let mut query = inbox_query(&self.account, mailbox);
        query.position = Some(position as i64);
        query.limit = Some(limit);
        query.calculate_total = Some(true);

        let mut b = self.client.batch();
        let q = b.call(EmailQuery {
            query,
            collapse_threads: true,
        })?;
        let reps = b.call(EmailGet::new(
            Get::new(self.account.clone())
                .ids_ref(q.result_ref("/ids"))
                .properties(vec![EmailProperty::Id, EmailProperty::ThreadId]),
        ))?;
        let threads = b.call(ThreadGet(
            Get::new(self.account.clone()).ids_ref(reps.result_ref("/list/*/threadId")),
        ))?;
        let emails = b.call(EmailGet::new(
            Get::new(self.account.clone())
                .ids_ref(threads.result_ref("/list/*/emailIds"))
                .properties(METADATA.to_vec()),
        ))?;
        let r = b.send().await?;
        let q = r.get(&q)?;
        let threads = r.get(&threads)?;
        let emails = match r.get(&emails) {
            Ok(e) => e,
            // Very long threads can exceed maxObjectsInGet; fetch in chunks instead.
            Err(jmap_http::Error::Method { error, .. })
                if error.kind == MethodErrorType::RequestTooLarge =>
            {
                let ids: Vec<Id> = threads
                    .list
                    .iter()
                    .flat_map(|t| t.email_ids.clone())
                    .collect();
                let list = self.fetch_metadata(&ids).await?;
                jmap_proto::GetResponse {
                    account_id: self.account.clone(),
                    state: self.current_email_state().await?,
                    list,
                    not_found: vec![],
                }
            }
            Err(e) => return Err(e.into()),
        };

        self.store.upsert_emails(&emails.list)?;
        self.store.upsert_threads(&threads.list)?;
        if self.store.state("Email")?.is_none() {
            self.store.set_state("Email", emails.state.as_str())?;
        }
        self.store.put_query_page(
            mailbox,
            q.query_state.as_str(),
            q.position as usize,
            &q.ids,
            q.total,
        )?;
        self.emit(StoreEvent::ListChanged(mailbox.clone()));
        Ok(())
    }

    async fn current_email_state(&self) -> Result<jmap_proto::State> {
        let mut b = self.client.batch();
        let g = b.call(EmailGet::new(
            Get::new(self.account.clone())
                .ids(vec![])
                .properties(vec![EmailProperty::Id]),
        ))?;
        Ok(b.send().await?.get(&g)?.state)
    }

    async fn fetch_metadata(&self, ids: &[Id]) -> Result<Vec<Email>> {
        self.fetch_emails(ids, METADATA).await
    }

    async fn fetch_emails(&self, ids: &[Id], props: &[EmailProperty]) -> Result<Vec<Email>> {
        let mut out = Vec::with_capacity(ids.len());
        for chunk in chunk_ids(ids, self.get_limit()) {
            let mut b = self.client.batch();
            let g = b.call(EmailGet::new(
                Get::new(self.account.clone())
                    .ids(chunk)
                    .properties(props.to_vec()),
            ))?;
            out.extend(b.send().await?.get(&g)?.list);
        }
        Ok(out)
    }

    /// Server-side full-text search, optionally within one mailbox. Matching
    /// emails and their threads are cached so the results render and open
    /// like any other list.
    pub async fn search(
        &self,
        text: &str,
        mailbox: Option<&Id>,
        position: u64,
        limit: u64,
    ) -> Result<SearchPage> {
        let filter: jmap_proto::Filter<EmailFilterCondition> = EmailFilterCondition {
            text: Some(text.to_owned()),
            in_mailbox: mailbox.cloned(),
            ..Default::default()
        }
        .into();
        let mut query = Query::new(self.account.clone());
        query.filter = Some(filter.clone());
        query.sort = Some(vec![Comparator::desc(EmailSortProperty::ReceivedAt)]);
        query.position = Some(position as i64);
        query.limit = Some(limit);
        query.calculate_total = Some(true);

        let mut b = self.client.batch();
        let q = b.call(EmailQuery {
            query,
            collapse_threads: true,
        })?;
        let reps = b.call(EmailGet::new(
            Get::new(self.account.clone())
                .ids_ref(q.result_ref("/ids"))
                .properties(vec![EmailProperty::Id, EmailProperty::ThreadId]),
        ))?;
        let threads = b.call(ThreadGet(
            Get::new(self.account.clone()).ids_ref(reps.result_ref("/list/*/threadId")),
        ))?;
        let emails = b.call(EmailGet::new(
            Get::new(self.account.clone())
                .ids_ref(threads.result_ref("/list/*/emailIds"))
                .properties(METADATA.to_vec()),
        ))?;
        let snippets = b.call(SearchSnippetGet {
            account_id: self.account.clone(),
            filter: Some(filter),
            email_ids: None,
            email_ids_ref: Some(q.result_ref("/ids")),
        })?;
        let r = b.send().await?;
        let q = r.get(&q)?;
        let threads = r.get(&threads)?;
        let emails = r.get(&emails)?;
        self.store.upsert_emails(&emails.list)?;
        self.store.upsert_threads(&threads.list)?;
        if self.store.state("Email")?.is_none() {
            self.store.set_state("Email", emails.state.as_str())?;
        }
        // Snippets are a nicety; a server without them still gives results.
        let snippets = match r.get(&snippets) {
            Ok(s) => s
                .list
                .into_iter()
                .map(|s| (s.email_id.clone(), s))
                .collect(),
            Err(e) => {
                tracing::debug!("no search snippets: {e}");
                Default::default()
            }
        };
        Ok(SearchPage {
            ids: q.ids,
            position: q.position,
            total: q.total,
            snippets,
        })
    }

    /// Bring a cached mailbox list up to date with `Email/queryChanges`, falling
    /// back to reloading the first page.
    pub async fn refresh_list(&self, mailbox: &Id) -> Result<()> {
        let Some(cache) = self.store.query(mailbox)? else {
            return self.load_mailbox(mailbox, 0, PAGE_SIZE).await;
        };
        let base = inbox_query(&self.account, mailbox);
        let mut b = self.client.batch();
        let qc = b.call(EmailQueryChanges {
            query_changes: QueryChanges {
                account_id: self.account.clone(),
                filter: base.filter,
                sort: base.sort,
                since_query_state: cache.query_state.as_str().into(),
                max_changes: Some(self.get_limit()),
                up_to_id: None,
                calculate_total: Some(true),
            },
            collapse_threads: true,
        })?;
        let r = b.send().await?;
        let changes = match r.get(&qc) {
            Err(e) if is_cannot_calculate(&e) => {
                let len = (cache.ids.len() as u64).clamp(PAGE_SIZE, 4 * PAGE_SIZE);
                return self.load_mailbox(mailbox, 0, len).await;
            }
            other => other?,
        };
        if changes.old_query_state == changes.new_query_state {
            return Ok(());
        }
        // Make sure every newly listed email (and its thread) is cached.
        let added: Vec<Id> = changes.added.iter().map(|a| a.id.clone()).collect();
        self.ensure_emails(&added).await?;
        self.store.apply_query_changes(
            mailbox,
            changes.new_query_state.as_str(),
            &changes.removed,
            &changes
                .added
                .iter()
                .map(|a| (a.id.clone(), a.index))
                .collect::<Vec<_>>(),
            changes.total,
        )?;
        self.emit(StoreEvent::ListChanged(mailbox.clone()));
        Ok(())
    }

    /// Fetch metadata for any of `ids` not in the store, plus their threads.
    async fn ensure_emails(&self, ids: &[Id]) -> Result<()> {
        let missing = self.store.missing_emails(ids)?;
        if !missing.is_empty() {
            let emails = self.fetch_metadata(&missing).await?;
            self.store.upsert_emails(&emails)?;
        }
        let threads = self.store.thread_ids_of(ids)?;
        self.refresh_threads(&threads).await
    }

    async fn refresh_threads(&self, thread_ids: &[Id]) -> Result<()> {
        if thread_ids.is_empty() {
            return Ok(());
        }
        let mut all = Vec::new();
        for chunk in chunk_ids(thread_ids, self.get_limit()) {
            let mut b = self.client.batch();
            let g = b.call(ThreadGet(Get::new(self.account.clone()).ids(chunk)))?;
            all.extend(b.send().await?.get(&g)?.list);
        }
        let member_ids: Vec<Id> = all.iter().flat_map(|t| t.email_ids.clone()).collect();
        self.store.upsert_threads(&all)?;
        let missing = self.store.missing_emails(&member_ids)?;
        if !missing.is_empty() {
            let emails = self.fetch_metadata(&missing).await?;
            self.store.upsert_emails(&emails)?;
        }
        Ok(())
    }

    /// Make sure every email in a thread is cached; returns them oldest first.
    pub async fn load_thread(&self, thread_id: &Id) -> Result<Vec<EmailMeta>> {
        let known = self.store.thread_email_ids(thread_id)?;
        let complete = match &known {
            Some(ids) => self.store.missing_emails(ids)?.is_empty(),
            None => false,
        };
        if !complete {
            self.refresh_threads(std::slice::from_ref(thread_id))
                .await?;
        }
        self.store.thread_emails(thread_id)
    }

    // ----- email delta sync ----------------------------------------------

    /// Apply `Email/changes` since the stored state. Falls back to a full resync
    /// when the server cannot calculate changes.
    pub async fn sync_emails(&self) -> Result<()> {
        let Some(mut since) = self.store.state("Email")? else {
            return Ok(()); // nothing cached yet
        };
        let mut changed_ids = Vec::new();
        loop {
            let mut b = self.client.batch();
            let ch = b.call(EmailChanges(Changes {
                account_id: self.account.clone(),
                since_state: since.as_str().into(),
                max_changes: Some(self.get_limit().min(256)),
            }))?;
            let created = b.call(EmailGet::new(
                Get::new(self.account.clone())
                    .ids_ref(ch.result_ref("/created"))
                    .properties(METADATA.to_vec()),
            ))?;
            let updated = b.call(EmailGet::new(
                Get::new(self.account.clone())
                    .ids_ref(ch.result_ref("/updated"))
                    .properties(FLAGS.to_vec()),
            ))?;
            let r = b.send().await?;
            let changes = match r.get(&ch) {
                Err(e) if is_cannot_calculate(&e) => return self.full_resync_emails().await,
                other => other?,
            };
            let created = r.get(&created)?.list;
            let updated = r.get(&updated)?.list;

            let mut threads = self.store.delete_emails(&changes.destroyed)?;
            threads.extend(created.iter().filter_map(|e| e.thread_id.clone()));
            self.store.upsert_emails(&created)?;
            self.store.update_email_flags(&updated)?;
            threads.sort();
            threads.dedup();
            self.refresh_threads(&threads).await?;
            self.store.set_state("Email", changes.new_state.as_str())?;

            changed_ids.extend(changes.created);
            changed_ids.extend(changes.updated);
            changed_ids.extend(changes.destroyed);
            if !changes.has_more_changes {
                break;
            }
            since = changes.new_state.0;
        }
        if !changed_ids.is_empty() {
            self.emit(StoreEvent::EmailsChanged(changed_ids));
        }
        Ok(())
    }

    async fn full_resync_emails(&self) -> Result<()> {
        tracing::info!("server cannot calculate email changes; resyncing cached lists");
        let mailboxes = self.store.clear_emails()?;
        for mb in &mailboxes {
            self.load_mailbox(mb, 0, PAGE_SIZE).await?;
        }
        self.emit(StoreEvent::EmailsChanged(Vec::new()));
        Ok(())
    }

    /// Full catch-up: mailboxes, email changes, then every cached list.
    pub async fn sync_all(&self) -> Result<()> {
        self.sync_mailboxes().await?;
        self.flush_pending().await?;
        self.sync_emails().await?;
        for mb in self.store.cached_query_mailboxes()? {
            self.refresh_list(&mb).await?;
        }
        Ok(())
    }

    // ----- bodies ---------------------------------------------------------

    /// The body of an email, from cache or fetched.
    pub async fn body(&self, id: &Id) -> Result<EmailBody> {
        if let Some(b) = self.store.body(id)? {
            return Ok(b);
        }
        let mut get = EmailGet::new(
            Get::new(self.account.clone())
                .ids(vec![id.clone()])
                .properties(vec![
                    EmailProperty::Id,
                    EmailProperty::TextBody,
                    EmailProperty::HtmlBody,
                    EmailProperty::Attachments,
                    EmailProperty::BodyValues,
                ]),
        );
        get.fetch_text_body_values = true;
        get.fetch_html_body_values = true;
        let mut b = self.client.batch();
        let h = b.call(get)?;
        let email = b
            .send()
            .await?
            .get(&h)?
            .list
            .into_iter()
            .next()
            .ok_or_else(|| Error::NotFound(format!("email {id}")))?;
        let body = EmailBody {
            text_body: email.text_body.unwrap_or_default(),
            html_body: email.html_body.unwrap_or_default(),
            attachments: email.attachments.unwrap_or_default(),
            body_values: email.body_values.unwrap_or_default(),
        };
        self.store.save_body(id, &body)?;
        self.emit(StoreEvent::BodyLoaded(id.clone()));
        Ok(body)
    }

    /// Every header field of an email, in message order, with raw values.
    pub async fn headers(&self, id: &Id) -> Result<Vec<EmailHeader>> {
        let email = self
            .fetch_emails(
                std::slice::from_ref(id),
                &[EmailProperty::Id, EmailProperty::Headers],
            )
            .await?
            .into_iter()
            .next()
            .ok_or_else(|| Error::NotFound(format!("email {id}")))?;
        Ok(email.headers.unwrap_or_default())
    }

    /// The complete RFC 5322 message as received.
    pub async fn raw_message(&self, id: &Id) -> Result<bytes::Bytes> {
        let blob = match self.store.email(id)?.and_then(|m| m.blob_id) {
            Some(b) => b,
            None => self
                .fetch_emails(
                    std::slice::from_ref(id),
                    &[EmailProperty::Id, EmailProperty::BlobId],
                )
                .await?
                .into_iter()
                .next()
                .and_then(|e| e.blob_id)
                .ok_or_else(|| Error::NotFound(format!("message source for {id}")))?,
        };
        self.blob(&blob, "message.eml", "message/rfc822").await
    }

    /// Download a blob (attachments, inline images).
    pub async fn blob(&self, blob: &Id, name: &str, ty: &str) -> Result<bytes::Bytes> {
        Ok(self.client.download(&self.account, blob, name, ty).await?)
    }

    // ----- triage ---------------------------------------------------------

    fn patches_for(&self, ids: &[Id], op: &TriageOp) -> Result<Vec<(Id, PatchObject)>> {
        let archive = match op {
            TriageOp::Archive { .. } => Some(
                self.store
                    .mailbox_by_role(&MailboxRole::Archive)?
                    .ok_or(Error::NoRoleMailbox("archive"))?,
            ),
            _ => None,
        };
        let trash = match op {
            TriageOp::Trash => Some(
                self.store
                    .mailbox_by_role(&MailboxRole::Trash)?
                    .ok_or(Error::NoRoleMailbox("trash"))?,
            ),
            _ => None,
        };
        let mut out = Vec::with_capacity(ids.len());
        for id in ids {
            let mut p = PatchObject::new();
            match op {
                TriageOp::SetSeen(on) => patch::keyword(&mut p, keyword::SEEN, *on),
                TriageOp::SetFlagged(on) => patch::keyword(&mut p, keyword::FLAGGED, *on),
                TriageOp::Move { from, to } => {
                    patch::mailbox(&mut p, from, false);
                    patch::mailbox(&mut p, to, true);
                }
                TriageOp::Archive { from } => {
                    let archive = archive.as_ref().expect("resolved above");
                    patch::mailbox(&mut p, from, false);
                    patch::mailbox(&mut p, archive, true);
                }
                TriageOp::Trash => {
                    patch::set_mailboxes(&mut p, [trash.as_ref().expect("resolved above")]);
                }
            }
            out.push((id.clone(), p));
        }
        Ok(out)
    }

    /// Apply a triage op optimistically: update the store at once, then tell the
    /// server. Rejected changes are rolled back; network failures are queued.
    pub async fn apply(&self, ids: &[Id], op: TriageOp) -> Result<()> {
        if ids.is_empty() {
            return Ok(());
        }
        let patches = self.patches_for(ids, &op)?;
        let snapshot = self.store.snapshot_flags(ids)?;
        self.store.apply_patches(&patches)?;
        self.emit(StoreEvent::EmailsChanged(ids.to_vec()));

        match self.send_patches(&patches).await {
            Ok(rejected) if rejected.is_empty() => Ok(()),
            Ok(rejected) => {
                let bad: Vec<_> = snapshot
                    .iter()
                    .filter(|s| rejected.iter().any(|(id, _)| *id == s.email_id))
                    .cloned()
                    .collect();
                self.store.restore_flags(&bad)?;
                self.emit(StoreEvent::EmailsChanged(
                    rejected.iter().map(|(id, _)| id.clone()).collect(),
                ));
                Err(Error::Rejected(rejected))
            }
            Err(e) if e.is_transient() => {
                tracing::info!("offline; queueing {} change(s): {e}", patches.len());
                self.store.queue_ops(&patches)?;
                self.emit(StoreEvent::Offline);
                Ok(())
            }
            Err(e) => {
                self.store.restore_flags(&snapshot)?;
                self.emit(StoreEvent::EmailsChanged(ids.to_vec()));
                Err(e)
            }
        }
    }

    /// Send patches in `maxObjectsInSet` chunks; returns per-email rejections.
    async fn send_patches(
        &self,
        patches: &[(Id, PatchObject)],
    ) -> Result<Vec<(Id, jmap_proto::SetError)>> {
        let max = self.client.session().core().max_objects_in_set;
        let mut rejected = Vec::new();
        for chunk in chunk_ids(patches, max) {
            let mut set = jmap_proto::Set::new(self.account.clone());
            for (id, p) in chunk {
                set = set.update(id, p);
            }
            let mut b = self.client.batch();
            let h = b.call(EmailSet(set))?;
            let r = b.send().await?.get(&h)?;
            rejected.extend(r.not_updated.unwrap_or_default());
        }
        Ok(rejected)
    }

    /// Send queued offline changes, oldest first.
    pub async fn flush_pending(&self) -> Result<()> {
        for (op, id, patch) in self.store.pending_ops()? {
            match self.send_patches(&[(id.clone(), patch)]).await {
                Ok(rejected) => {
                    self.store.remove_op(op)?;
                    if !rejected.is_empty() {
                        // The server said no; take its version.
                        let fresh = self.fetch_emails(std::slice::from_ref(&id), FLAGS).await?;
                        self.store.update_email_flags(&fresh)?;
                        self.emit(StoreEvent::EmailsChanged(vec![id]));
                    }
                }
                Err(e) if e.is_transient() => return Err(e),
                Err(e) => {
                    tracing::warn!("dropping queued change for {id}: {e}");
                    self.store.remove_op(op)?;
                }
            }
        }
        Ok(())
    }

    // ----- push -----------------------------------------------------------

    /// Run forever: hold an EventSource connection, sync on every state change,
    /// and reconnect with backoff. Returns only if authentication fails.
    pub async fn run_push(self: Arc<Self>) {
        const PING: u32 = 60;
        let mut backoff = Duration::from_secs(1);
        loop {
            match self.client.event_source(PING).await {
                Ok(mut stream) => {
                    backoff = Duration::from_secs(1);
                    self.emit(StoreEvent::Online);
                    if let Err(e) = self.sync_all().await {
                        self.report(e);
                    }
                    let idle = Duration::from_secs(u64::from(PING) * 2 + 10);
                    loop {
                        match tokio::time::timeout(idle, stream.next()).await {
                            Ok(Some(Ok(PushEvent::StateChange(sc)))) => {
                                let types: Vec<&str> =
                                    sc.types_for(&self.account).map(|(t, _)| t).collect();
                                if let Err(e) = self.on_state_change(&types).await {
                                    self.report(e);
                                }
                            }
                            Ok(Some(Ok(PushEvent::Ping))) => {}
                            Ok(Some(Err(e))) => {
                                tracing::debug!("push stream error: {e}");
                                break;
                            }
                            Ok(None) => break,
                            Err(_) => {
                                tracing::debug!("push stream idle; reconnecting");
                                break;
                            }
                        }
                    }
                }
                Err(jmap_http::Error::Unauthorized) => {
                    self.emit(StoreEvent::AuthFailed);
                    return;
                }
                Err(e) => tracing::debug!("push connect failed: {e}"),
            }
            self.emit(StoreEvent::Offline);
            tokio::time::sleep(backoff).await;
            backoff = (backoff * 2).min(Duration::from_secs(300));
        }
    }

    async fn on_state_change(&self, types: &[&str]) -> Result<()> {
        if types.contains(&"Mailbox") {
            self.sync_mailboxes().await?;
        }
        if types.contains(&"Email") || types.contains(&"Thread") {
            self.sync_emails().await?;
            for mb in self.store.cached_query_mailboxes()? {
                self.refresh_list(&mb).await?;
            }
        }
        Ok(())
    }

    fn report(&self, e: Error) {
        tracing::warn!("sync error: {e}");
        if matches!(e, Error::Jmap(jmap_http::Error::Unauthorized)) {
            self.emit(StoreEvent::AuthFailed);
        } else if e.is_transient() {
            self.emit(StoreEvent::Offline);
        } else {
            self.emit(StoreEvent::Error(e.to_string()));
        }
    }
}
