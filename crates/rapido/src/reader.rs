//! The thread reader. Embedded in the main window, or alone in a pop-out window.

use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::base::SelectableText;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::dialog::{DialogAction, DialogClose, DialogFooter};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::scroll::ScrollableElement;
use gpui_kit::component::text::TextView;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::*;
use gpui_kit::prelude::*;
use gpui_kit::*;
use jmap_proto::Id;
use jmap_proto::mail::EmailBodyPart;
use rapido_store::{EmailBody, EmailMeta, StoreEvent, SyncEngine, TriageOp, mailbox_tree};

use crate::actions::*;
use crate::attachments;
use crate::body_text::{find_links, reader_text, to_view_html};
use crate::browser_view::{self, Header, InlinePart};
use crate::format;
use crate::links::{self, LinkTarget};
use crate::services;

struct ReaderEmail {
    meta: EmailMeta,
    body: Option<EmailBody>,
    /// Pre-built HTML for the selectable text view.
    view_html: Option<SharedString>,
    has_html: bool,
    attachments: Vec<EmailBodyPart>,
}

impl ReaderEmail {
    fn new(meta: EmailMeta, body: Option<EmailBody>) -> Self {
        let mut e = Self {
            meta,
            body: None,
            view_html: None,
            has_html: false,
            attachments: Vec::new(),
        };
        if let Some(b) = body {
            e.set_body(b);
        }
        e
    }

    fn set_body(&mut self, body: EmailBody) {
        self.has_html = body.html().is_some();
        self.view_html = reader_text(&body).map(|t| to_view_html(&t, &find_links(&t)).into());
        self.attachments = attachments::listed(&body.attachments);
        self.body = Some(body);
    }
}

pub struct ReaderView {
    engine: Arc<SyncEngine>,
    /// Mailbox the thread was opened from; the "from" side of archive/move.
    mailbox: Option<Id>,
    thread_id: Option<Id>,
    emails: Vec<ReaderEmail>,
    loading: bool,
    gone: bool,
    /// Alone in its own window (pop-out).
    standalone: bool,
    title: Option<String>,
    status: Option<SharedString>,
    focus: FocusHandle,
    _tasks: Vec<Task<()>>,
}

impl Focusable for ReaderView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl ReaderView {
    pub fn new(engine: Arc<SyncEngine>, standalone: bool, cx: &mut Context<Self>) -> Self {
        let mut events = engine.subscribe();
        let listener = cx.spawn(async move |this, cx| {
            loop {
                let relevant = match events.recv().await {
                    Ok(StoreEvent::EmailsChanged(_)) => true,
                    Ok(_) => false,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => true,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                };
                if relevant && this.update(cx, |this, cx| this.refresh_flags(cx)).is_err() {
                    break;
                }
            }
        });
        Self {
            engine,
            mailbox: None,
            thread_id: None,
            emails: Vec::new(),
            loading: false,
            gone: false,
            standalone,
            title: None,
            status: None,
            focus: cx.focus_handle(),
            _tasks: vec![listener],
        }
    }

    /// Show a thread (or nothing). `mailbox` is where it was opened from.
    pub fn set_thread(&mut self, thread: Option<Id>, mailbox: Option<Id>, cx: &mut Context<Self>) {
        self.mailbox = mailbox;
        if self.thread_id == thread {
            return;
        }
        self.thread_id = thread.clone();
        self.gone = false;
        let Some(thread_id) = thread else {
            self.emails.clear();
            self.loading = false;
            cx.notify();
            return;
        };
        let store = self.engine.store().clone();
        self.emails = store
            .thread_emails(&thread_id)
            .unwrap_or_default()
            .into_iter()
            .map(|m| {
                let body = store.body(&m.id).ok().flatten();
                ReaderEmail::new(m, body)
            })
            .collect();
        self.loading = true;
        cx.notify();

        let engine = self.engine.clone();
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
                if this.thread_id.as_ref() != Some(&thread_id) {
                    return;
                }
                this.loading = false;
                match result {
                    Ok((metas, bodies)) => {
                        this.emails = metas
                            .into_iter()
                            .zip(bodies)
                            .map(|(m, b)| ReaderEmail::new(m, Some(b)))
                            .collect();
                        this.refresh_flags(cx);
                    }
                    Err(e) => this.report(e, cx),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn refresh_flags(&mut self, cx: &mut Context<Self>) {
        let store = self.engine.store().clone();
        for e in &mut self.emails {
            if let Ok(Some(m)) = store.email(&e.meta.id) {
                e.meta = m;
            }
        }
        if let Some(t) = &self.thread_id
            && !self.loading
            && store
                .thread_emails(t)
                .map(|v| v.is_empty())
                .unwrap_or(false)
        {
            self.gone = true;
        }
        cx.notify();
    }

    fn report(&mut self, e: anyhow::Error, cx: &mut Context<Self>) {
        tracing::warn!("{e:#}");
        self.status = Some(format!("{e:#}").into());
        cx.notify();
    }

    // ----- thread actions ---------------------------------------------------

    /// Emails of this thread, optionally only those in the opening mailbox.
    fn ids(&self, in_mailbox_only: bool) -> Vec<Id> {
        self.emails
            .iter()
            .filter(|e| {
                !in_mailbox_only
                    || self
                        .mailbox
                        .as_ref()
                        .is_none_or(|mb| e.meta.mailbox_ids.contains(mb))
            })
            .map(|e| e.meta.id.clone())
            .collect()
    }

    fn unread(&self) -> bool {
        self.emails.iter().any(|e| !e.meta.is_seen())
    }

    fn flagged(&self) -> bool {
        self.emails.iter().any(|e| e.meta.is_flagged())
    }

    fn triage(&self, ids: Vec<Id>, op: TriageOp, cx: &mut Context<Self>) {
        if ids.is_empty() {
            return;
        }
        let engine = self.engine.clone();
        cx.spawn(async move |this, cx| {
            if let Err(e) = services::run(async move { engine.apply(&ids, op).await }).await {
                let _ = this.update(cx, |this, cx| this.report(e, cx));
            }
        })
        .detach();
    }

    /// After moving the thread away, a pop-out window has nothing left to show.
    fn close_if_standalone(&self, window: &mut Window) {
        if self.standalone {
            window.remove_window();
        }
    }

    pub fn archive(&mut self, _: &Archive, window: &mut Window, cx: &mut Context<Self>) {
        let Some(from) = self.mailbox.clone() else {
            return;
        };
        self.triage(self.ids(true), TriageOp::Archive { from }, cx);
        self.close_if_standalone(window);
    }

    pub fn trash(&mut self, _: &Trash, window: &mut Window, cx: &mut Context<Self>) {
        self.triage(self.ids(true), TriageOp::Trash, cx);
        self.close_if_standalone(window);
    }

    pub fn toggle_flag(&mut self, _: &ToggleFlag, _: &mut Window, cx: &mut Context<Self>) {
        if self.flagged() {
            self.triage(self.ids(false), TriageOp::SetFlagged(false), cx);
        } else if let Some(last) = self.emails.last() {
            self.triage(vec![last.meta.id.clone()], TriageOp::SetFlagged(true), cx);
        }
    }

    pub fn toggle_read(&mut self, _: &ToggleRead, _: &mut Window, cx: &mut Context<Self>) {
        let unread = self.unread();
        self.triage(self.ids(false), TriageOp::SetSeen(unread), cx);
    }

    pub fn move_to(&mut self, _: &MoveTo, window: &mut Window, cx: &mut Context<Self>) {
        let Some(from) = self.mailbox.clone() else {
            return;
        };
        if self.emails.is_empty() {
            return;
        }
        let ids = self.ids(true);
        let standalone = self.standalone;
        let targets: Vec<(Id, SharedString, usize)> =
            mailbox_tree(self.engine.store().mailboxes().unwrap_or_default())
                .into_iter()
                .filter(|m| m.id != from)
                .map(|m| (m.id, m.name.into(), m.depth))
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
                            if standalone {
                                window.remove_window();
                            }
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

    pub fn open_in_new_window(
        &mut self,
        _: &OpenInNewWindow,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(thread) = self.thread_id.clone() else {
            return;
        };
        open_reader_window(self.engine.clone(), thread, self.mailbox.clone(), cx);
    }

    fn close_window(&mut self, _: &CloseWindow, window: &mut Window, _: &mut Context<Self>) {
        if self.standalone {
            window.remove_window();
        }
    }

    // ----- open in browser ---------------------------------------------------

    pub fn open_newest_in_browser(
        &mut self,
        _: &OpenInBrowser,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let id = self
            .emails
            .iter()
            .rev()
            .find(|e| e.has_html || e.body.is_none())
            .or(self.emails.last())
            .map(|e| e.meta.id.clone());
        if let Some(id) = id {
            self.open_in_browser(id, window, cx);
        }
    }

    fn open_in_browser(&mut self, id: Id, window: &mut Window, cx: &mut Context<Self>) {
        let Some(meta) = self
            .emails
            .iter()
            .find(|e| e.meta.id == id)
            .map(|e| e.meta.clone())
        else {
            return;
        };
        let engine = self.engine.clone();
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
                let inline: Vec<(String, EmailBodyPart)> = body
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
            let _ = this.update_in(cx, |_, window, cx| match prepared {
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
                Err(e) => window.push_notification(Notification::error(format!("{e:#}")), cx),
            });
        })
        .detach();
    }

    // ----- attachments ------------------------------------------------------

    fn save_attachment(
        &mut self,
        part: EmailBodyPart,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(blob) = part.blob_id.clone() else {
            return;
        };
        let name = attachments::file_name(&part);
        let mime = part
            .content_type
            .clone()
            .unwrap_or_else(|| "application/octet-stream".into());
        let chosen = cx.prompt_for_new_path(&attachments::default_dir(), Some(&name));
        let engine = self.engine.clone();
        cx.spawn_in(window, async move |_, cx| {
            let path: PathBuf = match chosen.await {
                Ok(Ok(Some(path))) => path,
                Ok(Ok(None)) | Err(_) => return,
                Ok(Err(e)) => {
                    let _ = cx.update(|window, cx| {
                        window.push_notification(
                            Notification::error(format!("Save dialog failed: {e:#}")),
                            cx,
                        )
                    });
                    return;
                }
            };
            let target = path.clone();
            let result = async {
                let bytes =
                    services::run(async move { engine.blob(&blob, &name, &mime).await }).await?;
                services::blocking(move || Ok(std::fs::write(&target, &bytes)?)).await
            }
            .await;
            let _ = cx.update(|window, cx| match result {
                Ok(()) => {
                    let reveal = path.clone();
                    window.push_notification(
                        Notification::success(format!("Saved {}", path.display()))
                            .on_click(move |_, _, cx| cx.reveal_path(&reveal)),
                        cx,
                    )
                }
                Err(e) => window
                    .push_notification(Notification::error(format!("Could not save: {e:#}")), cx),
            });
        })
        .detach();
    }
}

/// Open a thread in its own window.
pub fn open_reader_window(engine: Arc<SyncEngine>, thread: Id, mailbox: Option<Id>, cx: &mut App) {
    let bounds = Bounds::centered(None, size(px(760.), px(720.)), cx);
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        titlebar: Some(TitlebarOptions {
            title: Some("Rápido".into()),
            ..Default::default()
        }),
        window_min_size: Some(size(px(420.), px(320.))),
        app_id: Some("cl.kddlb.rapido".into()),
        ..Default::default()
    };
    let result = gpui_kit::open_window(options, cx, move |window, cx| {
        cx.new(|cx| {
            let mut reader = ReaderView::new(engine, true, cx);
            reader.set_thread(Some(thread), mailbox, cx);
            reader.focus.focus(window, cx);
            reader
        })
    });
    if let Err(e) = result {
        tracing::warn!("could not open reader window: {e:#}");
    }
}

/// Ask before opening any link from a message, showing its real destination.
pub fn confirm_open_link(target: LinkTarget, window: &mut Window, cx: &mut App) {
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
                        .child("Rápido only opens web and email links."),
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

impl ReaderView {
    fn render_toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let enabled = !self.emails.is_empty();
        let flagged = self.flagged();
        let unread = self.unread();
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
            .flex_none()
            .px_2()
            .gap_1()
            .border_b_1()
            .border_color(theme.border)
            .child(
                tool("archive", Icon::new(Lucide::Archive), "Archive (E)")
                    .disabled(!enabled || self.mailbox.is_none())
                    .on_click(cx.listener(|this, _, w, cx| this.archive(&Archive, w, cx))),
            )
            .child(
                tool("trash", Icon::new(Lucide::Trash), "Move to Trash (#)")
                    .on_click(cx.listener(|this, _, w, cx| this.trash(&Trash, w, cx))),
            )
            .child(
                tool("move", Icon::new(Lucide::FolderInput), "Move to… (V)")
                    .disabled(!enabled || self.mailbox.is_none())
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
            .when(!self.standalone, |d| {
                d.child(
                    tool(
                        "new-window",
                        Icon::new(Lucide::SquareArrowOutUpRight),
                        "Open in new window (Enter)",
                    )
                    .on_click(cx.listener(|this, _, w, cx| {
                        this.open_in_new_window(&OpenInNewWindow, w, cx)
                    })),
                )
            })
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
        let id = e.meta.id.clone();
        let body: AnyElement = match &e.view_html {
            Some(html) => TextView::html(SharedString::from(format!("body-{id}")), html.clone())
                .selectable(true)
                .on_link_click(|url, _, window, cx| {
                    confirm_open_link(links::inspect(url), window, cx)
                })
                .link_tooltip(|url, window, cx| {
                    let t = links::inspect(url);
                    let text = if t.idn {
                        format!("{}\nInternationalized domain, shown in punycode", t.display)
                    } else {
                        t.display
                    };
                    Tooltip::new(text).build(window, cx)
                })
                .font_family(theme.mono_font_family.clone())
                .text_size(theme.mono_font_size)
                .into_any_element(),
            None if e.body.is_some() => div()
                .text_color(theme.muted_foreground)
                .child("(no text)")
                .into_any_element(),
            None => div()
                .text_color(theme.muted_foreground)
                .child("Loading…")
                .into_any_element(),
        };
        let header_line = |key: &str, text: String| {
            div()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(SelectableText::new(
                    SharedString::from(format!("{key}-{id}")),
                    text,
                ))
        };
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
                            .child(div().text_sm().font_weight(FontWeight::SEMIBOLD).child(
                                SelectableText::new(
                                    SharedString::from(format!("from-{id}")),
                                    format::address_list(&e.meta.from),
                                ),
                            ))
                            .child(header_line(
                                "to",
                                format!("To: {}", format::address_list(&e.meta.to)),
                            ))
                            .when(!e.meta.cc.is_empty(), |d| {
                                d.child(header_line(
                                    "cc",
                                    format!("Cc: {}", format::address_list(&e.meta.cc)),
                                ))
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
                let id = id.clone();
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
            .child(body)
            .when(!e.attachments.is_empty(), |d| {
                d.child(h_flex().flex_wrap().gap_2().children(
                    e.attachments.iter().enumerate().map(|(i, part)| {
                        let part = part.clone();
                        let label = format!(
                            "{} · {}",
                            attachments::file_name(&part),
                            attachments::size(part.size.unwrap_or(0))
                        );
                        Button::new(SharedString::from(format!("att-{id}-{i}")))
                            .small()
                            .outline()
                            .icon(Icon::new(Lucide::Paperclip))
                            .label(label)
                            .tooltip("Save attachment…")
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.save_attachment(part.clone(), window, cx)
                            }))
                    }),
                ))
            })
    }
}

impl Render for ReaderView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let subject = self
            .emails
            .last()
            .map(|e| e.meta.subject.clone())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "(no subject)".to_owned());
        if self.standalone
            && !self.emails.is_empty()
            && self.title.as_deref() != Some(subject.as_str())
        {
            window.set_window_title(&format!("{subject} — Rápido"));
            self.title = Some(subject.clone());
        }
        let content: AnyElement = if self.thread_id.is_none() {
            v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child("Select a conversation")
                .into_any_element()
        } else if self.gone {
            v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child("This conversation no longer exists.")
                .into_any_element()
        } else {
            let emails: Vec<AnyElement> = self
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
                                .child(SelectableText::new("subject", subject)),
                        )
                        .when(self.loading && self.emails.is_empty(), |d| {
                            d.child(
                                div()
                                    .p_4()
                                    .child(gpui_kit::component::spinner::Spinner::new()),
                            )
                        })
                        .children(emails),
                )
                .into_any_element()
        };
        let root = v_flex()
            .size_full()
            .bg(theme.background)
            .text_color(theme.foreground)
            .child(self.render_toolbar(cx))
            .child(div().flex_1().min_h_0().child(content))
            .children(self.status.clone().map(|s| {
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
                        Button::new("dismiss-reader-status")
                            .ghost()
                            .xsmall()
                            .icon(IconName::Close)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.status = None;
                                cx.notify();
                            })),
                    )
            }));
        if self.standalone {
            root.key_context(READER_CONTEXT)
                .track_focus(&self.focus)
                .on_action(cx.listener(Self::archive))
                .on_action(cx.listener(Self::trash))
                .on_action(cx.listener(Self::toggle_flag))
                .on_action(cx.listener(Self::toggle_read))
                .on_action(cx.listener(Self::move_to))
                .on_action(cx.listener(Self::open_newest_in_browser))
                .on_action(cx.listener(Self::close_window))
        } else {
            root
        }
    }
}
