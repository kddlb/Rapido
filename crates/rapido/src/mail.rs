//! The main window: mailbox sidebar, thread list and reader.

use std::collections::HashMap;
use std::ops::Range;
use std::rc::Rc;
use std::sync::Arc;

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::dialog::{DialogAction, DialogClose, DialogFooter};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::scroll::ScrollableElement;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::*;
use gpui_kit::prelude::*;
use gpui_kit::*;
use jmap_proto::Id;
use jmap_proto::mail::MailboxRole;
use rapido_store::{
    EmailBody, EmailMeta, MailboxRow, PAGE_SIZE, StoreEvent, SyncEngine, ThreadRow, TriageOp,
    mailbox_tree,
};

use crate::actions::*;
use crate::body_text::{find_links, reader_text};
use crate::browser_view::{self, Header, InlinePart};
use crate::format;
use crate::links::{self, LinkTarget};
use crate::services::{self, Account};

const ROW_HEIGHT: f32 = 68.;

pub enum MailEvent {
    SignedOut { session_url: String },
}

struct ReaderEmail {
    meta: EmailMeta,
    body: Option<EmailBody>,
    text: Option<SharedString>,
    links: Vec<(Range<usize>, String)>,
    has_html: bool,
}

struct Reader {
    thread_id: Id,
    emails: Vec<ReaderEmail>,
    loading: bool,
}

pub struct MailView {
    account: Arc<Account>,
    focus: FocusHandle,
    mailboxes: Vec<MailboxRow>,
    mailbox: Option<Id>,
    rows: Vec<ThreadRow>,
    total: Option<u64>,
    selected: Option<usize>,
    list_scroll: UniformListScrollHandle,
    loading_list: bool,
    loading_more: bool,
    reader: Option<Reader>,
    online: Option<bool>,
    status: Option<SharedString>,
    push: tokio::task::AbortHandle,
}

impl EventEmitter<MailEvent> for MailView {}

impl Focusable for MailView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Drop for MailView {
    fn drop(&mut self) {
        self.push.abort();
    }
}

impl MailView {
    pub fn new(account: Arc<Account>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let engine = account.engine.clone();

        // Store events → UI.
        let mut events = engine.subscribe();
        cx.spawn(async move |this, cx| {
            loop {
                let ev = match events.recv().await {
                    Ok(ev) => Some(ev),
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => None,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                };
                if this
                    .update(cx, |this, cx| this.on_store_event(ev, cx))
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();

        // Push connection + background sync for the life of the view.
        let push = services::spawn_abortable(engine.clone().run_push());

        let focus = cx.focus_handle();
        focus.focus(window, cx);
        let mut this = Self {
            account,
            focus,
            mailboxes: Vec::new(),
            mailbox: None,
            rows: Vec::new(),
            total: None,
            selected: None,
            list_scroll: UniformListScrollHandle::new(),
            loading_list: false,
            loading_more: false,
            reader: None,
            online: None,
            status: None,
            push,
        };
        this.refresh_mailboxes(cx);
        this
    }

    fn engine(&self) -> Arc<SyncEngine> {
        self.account.engine.clone()
    }

    fn on_store_event(&mut self, ev: Option<StoreEvent>, cx: &mut Context<Self>) {
        match ev {
            // Lagged: we missed events; refresh everything.
            None => {
                self.refresh_mailboxes(cx);
                self.reload_rows(cx);
                self.refresh_reader_flags(cx);
            }
            Some(StoreEvent::MailboxesChanged) => self.refresh_mailboxes(cx),
            Some(StoreEvent::ListChanged(mb)) => {
                if self.mailbox.as_ref() == Some(&mb) {
                    self.reload_rows(cx);
                }
            }
            Some(StoreEvent::EmailsChanged(_)) => {
                self.reload_rows(cx);
                self.refresh_reader_flags(cx);
            }
            Some(StoreEvent::BodyLoaded(_)) => {}
            Some(StoreEvent::Online) => {
                self.online = Some(true);
                self.status = None;
            }
            Some(StoreEvent::Offline) => self.online = Some(false),
            Some(StoreEvent::AuthFailed) => {
                self.online = Some(false);
                self.status =
                    Some("The server rejected the token. Sign out and sign in again.".into());
            }
            Some(StoreEvent::Error(e)) => self.status = Some(e.into()),
        }
        cx.notify();
    }

    // ----- data ---------------------------------------------------------

    fn refresh_mailboxes(&mut self, cx: &mut Context<Self>) {
        match self.engine().store().mailboxes() {
            Ok(rows) => self.mailboxes = mailbox_tree(rows),
            Err(e) => self.status = Some(e.to_string().into()),
        }
        let still_exists = self
            .mailbox
            .as_ref()
            .is_some_and(|id| self.mailboxes.iter().any(|m| &m.id == id));
        if !still_exists {
            self.mailbox = None;
            let inbox = self
                .mailboxes
                .iter()
                .find(|m| m.role == Some(MailboxRole::Inbox))
                .or(self.mailboxes.first())
                .map(|m| m.id.clone());
            if let Some(id) = inbox {
                self.select_mailbox(id, cx);
            }
        }
        cx.notify();
    }

    fn select_mailbox(&mut self, id: Id, cx: &mut Context<Self>) {
        if self.mailbox.as_ref() == Some(&id) {
            return;
        }
        self.mailbox = Some(id.clone());
        self.selected = None;
        self.reader = None;
        self.reload_rows(cx);
        self.list_scroll.scroll_to_item(0, ScrollStrategy::Top);
        let has_cache = self.engine().store().query(&id).ok().flatten().is_some();
        self.loading_list = !has_cache;
        let engine = self.engine();
        cx.spawn(async move |this, cx| {
            let mb = id.clone();
            let result = services::run(async move {
                if has_cache {
                    engine.refresh_list(&mb).await
                } else {
                    engine.load_mailbox(&mb, 0, PAGE_SIZE).await
                }
            })
            .await;
            let _ = this.update(cx, |this, cx| {
                if this.mailbox.as_ref() == Some(&id) {
                    this.loading_list = false;
                    if let Err(e) = result {
                        this.report(e, cx);
                    }
                    this.reload_rows(cx);
                }
            });
        })
        .detach();
        cx.notify();
    }

    fn reload_rows(&mut self, cx: &mut Context<Self>) {
        let Some(mb) = self.mailbox.clone() else {
            self.rows.clear();
            return;
        };
        let store = self.engine().store().clone();
        let selected_thread = self.selected_row().map(|r| r.thread_id.clone());
        match store.thread_list(&mb) {
            Ok(rows) => self.rows = rows,
            Err(e) => self.status = Some(e.to_string().into()),
        }
        self.total = store.query(&mb).ok().flatten().and_then(|q| q.total);
        // Keep the same thread selected if it is still listed; otherwise keep the index.
        if let Some(t) = selected_thread {
            if let Some(ix) = self.rows.iter().position(|r| r.thread_id == t) {
                self.selected = Some(ix);
            } else if let Some(ix) = self.selected {
                if self.rows.is_empty() {
                    self.selected = None;
                    self.reader = None;
                } else {
                    let ix = ix.min(self.rows.len() - 1);
                    self.selected = Some(ix);
                    let thread = self.rows[ix].thread_id.clone();
                    self.open_thread(thread, cx);
                }
            }
        }
        cx.notify();
    }

    fn has_more(&self) -> bool {
        let cached = self
            .mailbox
            .as_ref()
            .and_then(|mb| self.engine().store().query(mb).ok().flatten())
            .map(|q| q.ids.len() as u64)
            .unwrap_or(0);
        self.total.is_some_and(|t| t > cached)
    }

    fn load_more(&mut self, cx: &mut Context<Self>) {
        if self.loading_more || !self.has_more() {
            return;
        }
        let Some(mb) = self.mailbox.clone() else {
            return;
        };
        let position = self
            .engine()
            .store()
            .query(&mb)
            .ok()
            .flatten()
            .map(|q| q.ids.len() as u64)
            .unwrap_or(0);
        self.loading_more = true;
        let engine = self.engine();
        cx.spawn(async move |this, cx| {
            let result =
                services::run(async move { engine.load_mailbox(&mb, position, PAGE_SIZE).await })
                    .await;
            let _ = this.update(cx, |this, cx| {
                this.loading_more = false;
                if let Err(e) = result {
                    this.report(e, cx);
                }
                this.reload_rows(cx);
            });
        })
        .detach();
    }

    fn selected_row(&self) -> Option<&ThreadRow> {
        self.selected.and_then(|ix| self.rows.get(ix))
    }

    fn select_row(&mut self, ix: usize, cx: &mut Context<Self>) {
        if ix >= self.rows.len() {
            return;
        }
        self.selected = Some(ix);
        self.list_scroll.scroll_to_item(ix, ScrollStrategy::Nearest);
        let thread = self.rows[ix].thread_id.clone();
        self.open_thread(thread, cx);
        cx.notify();
    }

    fn open_thread(&mut self, thread_id: Id, cx: &mut Context<Self>) {
        if self
            .reader
            .as_ref()
            .is_some_and(|r| r.thread_id == thread_id)
        {
            return;
        }
        let cached: Vec<ReaderEmail> = self
            .engine()
            .store()
            .thread_emails(&thread_id)
            .unwrap_or_default()
            .into_iter()
            .map(|meta| self.reader_email(meta))
            .collect();
        self.reader = Some(Reader {
            thread_id: thread_id.clone(),
            emails: cached,
            loading: true,
        });
        let engine = self.engine();
        cx.spawn(async move |this, cx| {
            let tid = thread_id.clone();
            let result = services::run(async move {
                let metas = engine.load_thread(&tid).await?;
                let mut bodies = Vec::with_capacity(metas.len());
                for m in &metas {
                    bodies.push(engine.body(&m.id).await?);
                }
                // Opening a thread marks it read.
                let unread: Vec<Id> = metas
                    .iter()
                    .filter(|m| !m.is_seen())
                    .map(|m| m.id.clone())
                    .collect();
                if !unread.is_empty() {
                    engine.apply(&unread, TriageOp::SetSeen(true)).await?;
                }
                Ok((metas, bodies))
            })
            .await;
            let _ = this.update(cx, |this, cx| {
                let Some(reader) = this.reader.as_mut().filter(|r| r.thread_id == thread_id) else {
                    return;
                };
                reader.loading = false;
                match result {
                    Ok((metas, bodies)) => {
                        let emails = metas
                            .into_iter()
                            .zip(bodies)
                            .map(|(meta, body)| {
                                let mut e = this.reader_email(meta);
                                e.set_body(body);
                                e
                            })
                            .collect();
                        if let Some(r) = this.reader.as_mut() {
                            r.emails = emails;
                        }
                        this.refresh_reader_flags(cx);
                    }
                    Err(e) => this.report(e, cx),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn reader_email(&self, meta: EmailMeta) -> ReaderEmail {
        let body = self.engine().store().body(&meta.id).ok().flatten();
        let mut e = ReaderEmail {
            meta,
            body: None,
            text: None,
            links: Vec::new(),
            has_html: false,
        };
        if let Some(b) = body {
            e.set_body(b);
        }
        e
    }

    /// Re-read keywords/mailboxes of the emails in the reader.
    fn refresh_reader_flags(&mut self, cx: &mut Context<Self>) {
        let store = self.engine().store().clone();
        if let Some(r) = self.reader.as_mut() {
            for e in &mut r.emails {
                if let Ok(Some(m)) = store.email(&e.meta.id) {
                    e.meta = m;
                }
            }
        }
        cx.notify();
    }

    fn report(&mut self, e: anyhow::Error, cx: &mut Context<Self>) {
        tracing::warn!("{e:#}");
        self.status = Some(format!("{e:#}").into());
        cx.notify();
    }

    // ----- triage ---------------------------------------------------------

    /// Emails of the selected thread, optionally only those in the current mailbox.
    fn thread_ids(&self, in_mailbox_only: bool) -> Vec<Id> {
        let Some(row) = self.selected_row() else {
            return Vec::new();
        };
        let emails = self
            .engine()
            .store()
            .thread_emails(&row.thread_id)
            .unwrap_or_default();
        let ids: Vec<Id> = emails
            .iter()
            .filter(|e| {
                !in_mailbox_only
                    || self
                        .mailbox
                        .as_ref()
                        .is_some_and(|mb| e.mailbox_ids.contains(mb))
            })
            .map(|e| e.id.clone())
            .collect();
        if ids.is_empty() {
            vec![row.email_id.clone()]
        } else {
            ids
        }
    }

    fn triage(&mut self, ids: Vec<Id>, op: TriageOp, cx: &mut Context<Self>) {
        if ids.is_empty() {
            return;
        }
        let engine = self.engine();
        cx.spawn(async move |this, cx| {
            let result = services::run(async move { engine.apply(&ids, op).await }).await;
            if let Err(e) = result {
                let _ = this.update(cx, |this, cx| this.report(e, cx));
            }
        })
        .detach();
    }

    fn archive(&mut self, _: &Archive, _: &mut Window, cx: &mut Context<Self>) {
        let Some(from) = self.mailbox.clone() else {
            return;
        };
        let ids = self.thread_ids(true);
        self.triage(ids, TriageOp::Archive { from }, cx);
    }

    fn trash(&mut self, _: &Trash, _: &mut Window, cx: &mut Context<Self>) {
        let ids = self.thread_ids(true);
        self.triage(ids, TriageOp::Trash, cx);
    }

    fn toggle_flag(&mut self, _: &ToggleFlag, _: &mut Window, cx: &mut Context<Self>) {
        let Some(row) = self.selected_row().cloned() else {
            return;
        };
        if row.flagged {
            let ids = self.thread_ids(false);
            self.triage(ids, TriageOp::SetFlagged(false), cx);
        } else {
            self.triage(vec![row.email_id], TriageOp::SetFlagged(true), cx);
        }
    }

    fn toggle_read(&mut self, _: &ToggleRead, _: &mut Window, cx: &mut Context<Self>) {
        let Some(row) = self.selected_row().cloned() else {
            return;
        };
        let ids = self.thread_ids(false);
        self.triage(ids, TriageOp::SetSeen(row.unread), cx);
    }

    fn next_thread(&mut self, _: &NextThread, _: &mut Window, cx: &mut Context<Self>) {
        let ix = self
            .selected
            .map_or(0, |i| (i + 1).min(self.rows.len().saturating_sub(1)));
        self.select_row(ix, cx);
        if ix + 10 >= self.rows.len() {
            self.load_more(cx);
        }
    }

    fn prev_thread(&mut self, _: &PrevThread, _: &mut Window, cx: &mut Context<Self>) {
        let ix = self.selected.map_or(0, |i| i.saturating_sub(1));
        self.select_row(ix, cx);
    }

    fn refresh(&mut self, _: &Refresh, _: &mut Window, cx: &mut Context<Self>) {
        let engine = self.engine();
        cx.spawn(async move |this, cx| {
            if let Err(e) = services::run(async move { engine.sync_all().await }).await {
                let _ = this.update(cx, |this, cx| this.report(e, cx));
            }
        })
        .detach();
    }

    fn move_to(&mut self, _: &MoveTo, window: &mut Window, cx: &mut Context<Self>) {
        let Some(from) = self.mailbox.clone() else {
            return;
        };
        if self.selected_row().is_none() {
            return;
        }
        let ids = self.thread_ids(true);
        let targets: Vec<(Id, SharedString, usize)> = self
            .mailboxes
            .iter()
            .filter(|m| m.id != from)
            .map(|m| (m.id.clone(), m.name.clone().into(), m.depth))
            .collect();
        let view = cx.entity().downgrade();
        window.open_dialog(cx, move |dialog, _, cx| {
            let theme = cx.theme().clone();
            let list = v_flex()
                .gap_1()
                .children(targets.iter().map(|(id, name, depth)| {
                    let (id, ids, from, view) =
                        (id.clone(), ids.clone(), from.clone(), view.clone());
                    Button::new(SharedString::from(format!("move-{id}")))
                        .ghost()
                        .w_full()
                        .justify_start()
                        .child(div().pl(px(*depth as f32 * 14.)).child(name.clone()))
                        .on_click(move |_, window, cx| {
                            let (id, ids, from) = (id.clone(), ids.clone(), from.clone());
                            let _ = view.update(cx, |this, cx| {
                                this.triage(ids, TriageOp::Move { from, to: id }, cx)
                            });
                            window.close_dialog(cx);
                        })
                }));
            dialog
                .title("Move to…")
                .child(
                    div()
                        .id("move-targets")
                        .max_h(px(360.))
                        .overflow_y_scrollbar()
                        .border_1()
                        .border_color(theme.border)
                        .rounded(theme.radius)
                        .p_1()
                        .child(list),
                )
                .footer(
                    DialogFooter::new()
                        .child(DialogClose::new().trigger(|button| button.label("Cancel"))),
                )
        });
    }

    fn sign_out(&mut self, cx: &mut Context<Self>) {
        self.push.abort();
        let config = self.account.config.clone();
        let session_url = config.session_url.clone();
        cx.spawn(async move |this, cx| {
            let _ = services::blocking(move || config.forget()).await;
            let _ = this.update(cx, |_, cx| cx.emit(MailEvent::SignedOut { session_url }));
        })
        .detach();
    }

    // ----- open in browser -----------------------------------------------

    fn open_newest_in_browser(
        &mut self,
        _: &OpenInBrowser,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let id = self
            .reader
            .as_ref()
            .and_then(|r| {
                r.emails
                    .iter()
                    .rev()
                    .find(|e| e.has_html || e.body.is_none())
            })
            .map(|e| e.meta.id.clone());
        if let Some(id) = id {
            self.open_in_browser(id, window, cx);
        }
    }

    fn open_in_browser(&mut self, id: Id, window: &mut Window, cx: &mut Context<Self>) {
        let Some(meta) = self
            .reader
            .as_ref()
            .and_then(|r| r.emails.iter().find(|e| e.meta.id == id))
            .map(|e| e.meta.clone())
        else {
            return;
        };
        let engine = self.engine();
        cx.spawn_in(window, async move |this, cx| {
            let prepared = services::run(async move {
                let body = engine.body(&meta.id).await?;
                let html = body.html().or_else(|| {
                    body.plain_text().map(|t| {
                        format!(
                            "<pre style=\"white-space:pre-wrap\">{}</pre>",
                            ammonia::clean_text(&t)
                        )
                    })
                });
                let inline: Vec<(String, jmap_proto::mail::EmailBodyPart)> = body
                    .inline_parts()
                    .map(|(cid, p)| (cid.to_owned(), p.clone()))
                    .collect();
                let mut parts = HashMap::new();
                for (cid, part) in inline {
                    let (Some(blob), Some(mime)) =
                        (part.blob_id.as_ref(), part.content_type.as_deref())
                    else {
                        continue;
                    };
                    let name = part.name.clone().unwrap_or_else(|| "inline".into());
                    match engine.blob(blob, &name, mime).await {
                        Ok(bytes) => {
                            parts.insert(
                                cid,
                                InlinePart {
                                    mime: mime.to_owned(),
                                    bytes: bytes.to_vec(),
                                },
                            );
                        }
                        Err(e) => tracing::warn!("inline part {cid}: {e}"),
                    }
                }
                Ok((meta, html.unwrap_or_default(), parts))
            })
            .await;
            let _ = this.update_in(cx, |this, window, cx| match prepared {
                Ok((meta, html, parts)) => {
                    let page = Rc::new(PageInputs {
                        header: Header {
                            from: format::address_list(&meta.from),
                            to: format::address_list(&meta.to),
                            subject: meta.subject.clone(),
                            date: format::full_date(meta.received_at),
                        },
                        html,
                        parts,
                    });
                    if browser_view::has_remote_content(&browser_view::sanitize(&page.html)) {
                        ask_remote_then_open(page, window, cx);
                    } else {
                        write_and_open(&page, false, window, cx);
                    }
                }
                Err(e) => this.report(e, cx),
            });
        })
        .detach();
    }
}

impl ReaderEmail {
    fn set_body(&mut self, body: EmailBody) {
        self.has_html = body.html().is_some();
        let text = reader_text(&body);
        self.links = text.as_deref().map(find_links).unwrap_or_default();
        self.text = text.map(Into::into);
        self.body = Some(body);
    }
}

/// Ask before opening any link from a message, showing its real destination.
fn confirm_open_link(target: LinkTarget, window: &mut Window, cx: &mut App) {
    window.open_alert_dialog(cx, move |dialog, _, cx| {
        let theme = cx.theme().clone();
        let detail = v_flex()
            .gap_2()
            .child(
                div()
                    .p_2()
                    .rounded(theme.radius)
                    .bg(theme.muted)
                    .font_family(theme.mono_font_family.clone())
                    .text_sm()
                    .child(target.display.clone()),
            )
            .when(target.idn, |d| {
                d.child(div().text_sm().text_color(theme.warning).child(
                    "This address uses an internationalized domain name, shown here in its \
                     punycode form. Look-alike domains are a common phishing trick.",
                ))
            })
            .when(!target.openable, |d| {
                d.child(
                    div()
                        .text_sm()
                        .text_color(theme.danger)
                        .child("Rapido only opens web and email links."),
                )
            });
        let dialog = dialog.title("Open link?").description(detail);
        if target.openable {
            let url = target.url.clone();
            dialog.confirm().ok_text("Open").on_ok(move |_, _, cx| {
                cx.open_url(&url);
                true
            })
        } else {
            dialog.show_cancel(false).ok_text("Close")
        }
    });
}

struct PageInputs {
    header: Header,
    html: String,
    parts: HashMap<String, InlinePart>,
}

fn write_and_open(page: &PageInputs, allow_remote: bool, window: &mut Window, cx: &mut App) {
    let html = browser_view::build_page(&page.html, &page.header, &page.parts, allow_remote);
    match browser_view::view_dir().and_then(|dir| browser_view::write_page_in(&dir, &html)) {
        Ok(path) => cx.open_with_system(&path),
        Err(e) => window.push_notification(
            Notification::error(format!("Could not open the message: {e:#}")),
            cx,
        ),
    }
}

fn ask_remote_then_open(page: Rc<PageInputs>, window: &mut Window, cx: &mut App) {
    window.open_dialog(cx, move |dialog, _, cx| {
        let load = page.clone();
        let block = page.clone();
        dialog
            .title(browser_view::REMOTE_TITLE)
            .width(px(480.))
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(browser_view::REMOTE_MESSAGE),
            )
            .footer(
                DialogFooter::new()
                    .child(DialogClose::new().trigger(|button| button.label("Cancel")))
                    .child(
                        Button::new("load-remote")
                            .label("Load remote images")
                            .on_click(move |_, window, cx| {
                                window.close_dialog(cx);
                                write_and_open(&load, true, window, cx);
                            }),
                    )
                    .child(
                        DialogAction::new().child(
                            Button::new("block-remote")
                                .primary()
                                .label("Open without remote images"),
                        ),
                    ),
            )
            .on_ok(move |_, window, cx| {
                write_and_open(&block, false, window, cx);
                true
            })
    });
}

// ----- rendering ------------------------------------------------------------

fn role_icon(role: Option<&MailboxRole>) -> Icon {
    match role {
        Some(MailboxRole::Inbox) => Icon::new(IconName::Inbox),
        Some(MailboxRole::Archive) => Icon::new(Lucide::Archive),
        Some(MailboxRole::Trash) => Icon::new(Lucide::Trash),
        Some(MailboxRole::Sent) => Icon::new(Lucide::Send),
        Some(MailboxRole::Drafts) => Icon::new(IconName::FileText),
        Some(MailboxRole::Junk) => Icon::new(IconName::Ban),
        Some(MailboxRole::Flagged) => Icon::new(Lucide::Flag),
        _ => Icon::new(IconName::Folder),
    }
}

impl MailView {
    fn render_sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let items = self.mailboxes.iter().map(|m| {
            let selected = self.mailbox.as_ref() == Some(&m.id);
            let id = m.id.clone();
            h_flex()
                .id(SharedString::from(format!("mb-{}", m.id)))
                .gap_2()
                .h(px(30.))
                .pl(px(10. + m.depth as f32 * 14.))
                .pr_2()
                .mx_1()
                .rounded(theme.radius)
                .cursor_pointer()
                .when(selected, |d| {
                    d.bg(theme.sidebar_accent)
                        .text_color(theme.sidebar_accent_foreground)
                })
                .when(!selected, |d| d.hover(|s| s.bg(theme.list_hover)))
                .child(
                    role_icon(m.role.as_ref())
                        .small()
                        .text_color(theme.muted_foreground),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_sm()
                        .child(m.name.clone()),
                )
                .when(m.unread_emails > 0, |d| {
                    d.child(
                        div()
                            .text_xs()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.primary)
                            .child(m.unread_emails.to_string()),
                    )
                })
                .on_click(cx.listener(move |this, _, _, cx| this.select_mailbox(id.clone(), cx)))
        });
        let offline = self.online == Some(false);
        v_flex()
            .size_full()
            .bg(theme.sidebar)
            .text_color(theme.sidebar_foreground)
            .border_r_1()
            .border_color(theme.sidebar_border)
            .child(
                div()
                    .px_3()
                    .pt_3()
                    .pb_2()
                    .text_lg()
                    .font_weight(FontWeight::BOLD)
                    .child("Rapido"),
            )
            .child(
                div()
                    .id("mailboxes")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scrollbar()
                    .child(v_flex().gap_0p5().py_1().children(items)),
            )
            .child(
                v_flex()
                    .gap_1()
                    .p_2()
                    .border_t_1()
                    .border_color(theme.sidebar_border)
                    .child(
                        h_flex()
                            .gap_1()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .when(offline, |d| {
                                d.child(Icon::new(Lucide::WifiOff).xsmall())
                                    .child("Offline · ")
                            })
                            .child(div().truncate().child(self.account.username().to_owned())),
                    )
                    .child(
                        Button::new("sign-out")
                            .ghost()
                            .xsmall()
                            .icon(Icon::new(Lucide::LogOut))
                            .label("Sign out")
                            .on_click(cx.listener(|this, _, _, cx| this.sign_out(cx))),
                    ),
            )
    }

    fn render_row(&self, ix: usize, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let row = &self.rows[ix];
        let selected = self.selected == Some(ix);
        let now = chrono::Local::now();
        let weight = if row.unread {
            FontWeight::SEMIBOLD
        } else {
            FontWeight::NORMAL
        };
        h_flex()
            .id(ix)
            .h(px(ROW_HEIGHT))
            .px_3()
            .gap_2()
            .border_b_1()
            .border_color(theme.border)
            .cursor_pointer()
            .when(selected, |d| d.bg(theme.list_active))
            .when(!selected, |d| d.hover(|s| s.bg(theme.list_hover)))
            .child(
                div()
                    .w(px(8.))
                    .h(px(8.))
                    .rounded_full()
                    .when(row.unread, |d| d.bg(theme.primary)),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_0p5()
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_sm()
                                    .font_weight(weight)
                                    .child(format::senders(&row.senders)),
                            )
                            .when(row.count > 1, |d| {
                                d.child(
                                    div()
                                        .text_xs()
                                        .text_color(theme.muted_foreground)
                                        .child(row.count.to_string()),
                                )
                            })
                            .when(row.flagged, |d| {
                                d.child(Icon::new(Lucide::Flag).xsmall().text_color(theme.warning))
                            })
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .child(format::list_date(row.received_at, now)),
                            ),
                    )
                    .child(div().truncate().text_sm().font_weight(weight).child(
                        if row.subject.is_empty() {
                            "(no subject)".to_owned()
                        } else {
                            row.subject.clone()
                        },
                    ))
                    .child(
                        div()
                            .truncate()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(row.preview.clone()),
                    ),
            )
            .on_click(cx.listener(move |this, _, _, cx| this.select_row(ix, cx)))
            .into_any_element()
    }

    fn render_list(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let count = self.rows.len();
        let body = if count == 0 {
            v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child(if self.loading_list {
                    "Loading…"
                } else {
                    "No messages"
                })
                .into_any_element()
        } else {
            uniform_list(
                "threads",
                count,
                cx.processor(|this, range: Range<usize>, _window, cx| {
                    if range.end + 10 >= this.rows.len() {
                        this.load_more(cx);
                    }
                    range.map(|ix| this.render_row(ix, cx)).collect::<Vec<_>>()
                }),
            )
            .track_scroll(&self.list_scroll)
            .size_full()
            .into_any_element()
        };
        let name = self
            .mailbox
            .as_ref()
            .and_then(|id| self.mailboxes.iter().find(|m| &m.id == id))
            .map(|m| m.name.clone())
            .unwrap_or_default();
        v_flex()
            .size_full()
            .child(
                h_flex()
                    .h(px(44.))
                    .px_3()
                    .gap_2()
                    .border_b_1()
                    .border_color(theme.border)
                    .child(div().flex_1().font_weight(FontWeight::SEMIBOLD).child(name))
                    .when(self.loading_more || self.loading_list, |d| {
                        d.child(gpui_kit::component::spinner::Spinner::new().small())
                    })
                    .child(
                        Button::new("refresh")
                            .ghost()
                            .small()
                            .icon(IconName::RefreshCw)
                            .tooltip("Refresh (Ctrl+R)")
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.refresh(&Refresh, window, cx)
                            })),
                    ),
            )
            .child(div().flex_1().min_h_0().child(body))
    }

    fn render_toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let row = self.selected_row();
        let enabled = row.is_some();
        let flagged = row.is_some_and(|r| r.flagged);
        let unread = row.is_some_and(|r| r.unread);
        let tool = |id: &'static str, icon: Icon, tip: &'static str| {
            Button::new(id)
                .ghost()
                .small()
                .icon(icon)
                .tooltip(tip)
                .disabled(!enabled)
        };
        h_flex()
            .h(px(44.))
            .px_2()
            .gap_1()
            .border_b_1()
            .border_color(theme.border)
            .child(
                tool("archive", Icon::new(Lucide::Archive), "Archive (E)")
                    .on_click(cx.listener(|this, _, w, cx| this.archive(&Archive, w, cx))),
            )
            .child(
                tool("trash", Icon::new(Lucide::Trash), "Move to Trash (#)")
                    .on_click(cx.listener(|this, _, w, cx| this.trash(&Trash, w, cx))),
            )
            .child(
                tool("move", Icon::new(Lucide::FolderInput), "Move to… (V)")
                    .on_click(cx.listener(|this, _, w, cx| this.move_to(&MoveTo, w, cx))),
            )
            .child(
                tool(
                    "flag",
                    Icon::new(if flagged {
                        Lucide::FlagOff
                    } else {
                        Lucide::Flag
                    }),
                    if flagged { "Unflag (S)" } else { "Flag (S)" },
                )
                .on_click(cx.listener(|this, _, w, cx| this.toggle_flag(&ToggleFlag, w, cx))),
            )
            .child(
                tool(
                    "read",
                    Icon::new(if unread {
                        Lucide::MailOpen
                    } else {
                        Lucide::Mail
                    }),
                    if unread {
                        "Mark as read (U)"
                    } else {
                        "Mark as unread (U)"
                    },
                )
                .on_click(cx.listener(|this, _, w, cx| this.toggle_read(&ToggleRead, w, cx))),
            )
            .child(div().flex_1())
            .child(
                tool(
                    "browser",
                    Icon::new(IconName::ExternalLink),
                    "Open in browser (O)",
                )
                .on_click(
                    cx.listener(|this, _, w, cx| {
                        this.open_newest_in_browser(&OpenInBrowser, w, cx)
                    }),
                ),
            )
    }

    fn render_email(&self, ix: usize, e: &ReaderEmail, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let link_style = HighlightStyle {
            color: Some(theme.link),
            underline: Some(UnderlineStyle {
                thickness: px(1.),
                ..Default::default()
            }),
            ..Default::default()
        };
        let body: AnyElement = match &e.text {
            Some(text) => {
                let ranges: Vec<Range<usize>> = e.links.iter().map(|(r, _)| r.clone()).collect();
                let targets: Rc<Vec<LinkTarget>> =
                    Rc::new(e.links.iter().map(|(_, u)| links::inspect(u)).collect());
                let styled = StyledText::new(text.clone())
                    .with_highlights(ranges.iter().cloned().map(|r| (r, link_style)));
                let tip_targets = targets.clone();
                let tip_ranges = ranges.clone();
                InteractiveText::new(SharedString::from(format!("body-{}", e.meta.id)), styled)
                    .tooltip(move |ix, window, cx| {
                        let i = tip_ranges.iter().position(|r| r.contains(&ix))?;
                        let t = &tip_targets[i];
                        let text = if t.idn {
                            format!("{}\nInternationalized domain, shown in punycode", t.display)
                        } else {
                            t.display.clone()
                        };
                        Some(Tooltip::new(text).build(window, cx))
                    })
                    .on_click(ranges, move |i, window, cx| {
                        if let Some(t) = targets.get(i) {
                            confirm_open_link(t.clone(), window, cx);
                        }
                    })
                    .into_any_element()
            }
            None if e.body.is_some() => div()
                .text_color(theme.muted_foreground)
                .child("(no text)")
                .into_any_element(),
            None => div()
                .text_color(theme.muted_foreground)
                .child("Loading…")
                .into_any_element(),
        };
        let id = e.meta.id.clone();
        v_flex()
            .gap_2()
            .p_4()
            .border_b_1()
            .border_color(theme.border)
            .child(
                h_flex()
                    .gap_2()
                    .items_start()
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(format::address_list(&e.meta.from)),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .child(format!("To: {}", format::address_list(&e.meta.to))),
                            )
                            .when(!e.meta.cc.is_empty(), |d| {
                                d.child(
                                    div()
                                        .text_xs()
                                        .text_color(theme.muted_foreground)
                                        .child(format!("Cc: {}", format::address_list(&e.meta.cc))),
                                )
                            }),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(format::full_date(e.meta.received_at)),
                    ),
            )
            .when(e.has_html, |d| {
                d.child(
                    h_flex()
                        .gap_2()
                        .px_2()
                        .py_1()
                        .rounded(theme.radius)
                        .bg(theme.muted)
                        .text_xs()
                        .child(
                            div()
                                .flex_1()
                                .child("This message has formatting best viewed in a browser."),
                        )
                        .child(
                            Button::new(("open-browser", ix))
                                .xsmall()
                                .label("Open in browser")
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.open_in_browser(id.clone(), window, cx)
                                })),
                        ),
                )
            })
            .child(
                div()
                    .font_family(theme.mono_font_family.clone())
                    .text_size(theme.mono_font_size)
                    .child(body),
            )
            .when(
                e.body.as_ref().is_some_and(|b| !b.attachments.is_empty()),
                |d| {
                    let names: Vec<String> = e
                        .body
                        .as_ref()
                        .map(|b| {
                            b.attachments
                                .iter()
                                .filter(|a| a.disposition.as_deref() != Some("inline"))
                                .map(|a| a.name.clone().unwrap_or_else(|| "attachment".into()))
                                .collect()
                        })
                        .unwrap_or_default();
                    d.when(!names.is_empty(), |d| {
                        d.child(
                            div()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(format!("Attachments: {}", names.join(", "))),
                        )
                    })
                },
            )
    }

    fn render_reader(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let content: AnyElement = match &self.reader {
            None => v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child("Select a conversation")
                .into_any_element(),
            Some(r) => {
                let subject = r
                    .emails
                    .last()
                    .map(|e| e.meta.subject.clone())
                    .or_else(|| self.selected_row().map(|row| row.subject.clone()))
                    .unwrap_or_default();
                let emails: Vec<AnyElement> = r
                    .emails
                    .iter()
                    .enumerate()
                    .map(|(ix, e)| self.render_email(ix, e, cx).into_any_element())
                    .collect();
                div()
                    .id("reader")
                    .size_full()
                    .overflow_y_scrollbar()
                    .child(
                        v_flex()
                            .child(
                                div()
                                    .px_4()
                                    .pt_4()
                                    .pb_2()
                                    .text_xl()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(if subject.is_empty() {
                                        "(no subject)".to_owned()
                                    } else {
                                        subject
                                    }),
                            )
                            .when(r.loading && r.emails.is_empty(), |d| {
                                d.child(
                                    div()
                                        .p_4()
                                        .child(gpui_kit::component::spinner::Spinner::new()),
                                )
                            })
                            .children(emails),
                    )
                    .into_any_element()
            }
        };
        v_flex()
            .size_full()
            .child(self.render_toolbar(cx))
            .child(div().flex_1().min_h_0().child(content))
    }
}

impl Render for MailView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let status = self.status.clone();
        v_flex()
            .size_full()
            .key_context(MAIL_CONTEXT)
            .track_focus(&self.focus)
            .on_action(cx.listener(Self::next_thread))
            .on_action(cx.listener(Self::prev_thread))
            .on_action(cx.listener(Self::archive))
            .on_action(cx.listener(Self::trash))
            .on_action(cx.listener(Self::toggle_flag))
            .on_action(cx.listener(Self::toggle_read))
            .on_action(cx.listener(Self::move_to))
            .on_action(cx.listener(Self::open_newest_in_browser))
            .on_action(cx.listener(Self::refresh))
            .child(
                h_resizable("mail-panes")
                    .child(
                        resizable_panel()
                            .size(px(220.))
                            .size_range(px(160.)..px(400.))
                            .child(self.render_sidebar(cx)),
                    )
                    .child(
                        resizable_panel()
                            .size(px(380.))
                            .size_range(px(260.)..px(700.))
                            .child(
                                div()
                                    .size_full()
                                    .border_r_1()
                                    .border_color(theme.border)
                                    .child(self.render_list(cx)),
                            ),
                    )
                    .child(resizable_panel().child(self.render_reader(cx))),
            )
            .children(status.map(|s| {
                h_flex()
                    .px_3()
                    .py_1()
                    .gap_2()
                    .text_xs()
                    .bg(theme.muted)
                    .border_t_1()
                    .border_color(theme.border)
                    .child(div().flex_1().truncate().child(s))
                    .child(
                        Button::new("dismiss-status")
                            .ghost()
                            .xsmall()
                            .icon(IconName::Close)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.status = None;
                                cx.notify();
                            })),
                    )
            }))
    }
}
