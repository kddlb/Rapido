//! The main window: mailbox sidebar, thread list and an embedded reader.

use std::collections::HashMap;
use std::ops::Range;
use std::sync::Arc;

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::scroll::ScrollableElement;
use gpui_kit::component::*;
use gpui_kit::prelude::*;
use gpui_kit::*;
use jmap_proto::Id;
use jmap_proto::mail::{MailboxRole, SearchSnippet};
use rapido_store::{MailboxRow, PAGE_SIZE, StoreEvent, SyncEngine, ThreadRow, mailbox_tree};

use crate::actions::*;
use crate::browser_view;
use crate::config::Config;
use crate::format;
use crate::reader::{ReaderView, open_reader_window};
use crate::services::{self, Account};

const ROW_HEIGHT: f32 = 68.;

pub enum MailEvent {
    /// `error` is set when local data could not be fully removed.
    SignedOut {
        session_url: String,
        error: Option<String>,
    },
}

/// An active search: results replace the mailbox list until cleared.
struct Search {
    query: String,
    /// `None` searches all mail.
    scope: Option<Id>,
    ids: Vec<Id>,
    total: Option<u64>,
    snippets: HashMap<Id, SearchSnippet>,
    loading: bool,
    generation: u64,
}

pub struct MailView {
    account: Arc<Account>,
    search_input: Entity<InputState>,
    search_all: bool,
    search: Option<Search>,
    search_generation: u64,
    _subs: Vec<Subscription>,
    focus: FocusHandle,
    mailboxes: Vec<MailboxRow>,
    mailbox: Option<Id>,
    rows: Vec<ThreadRow>,
    total: Option<u64>,
    selected: Option<usize>,
    list_scroll: UniformListScrollHandle,
    loading_list: bool,
    loading_more: bool,
    reader: Entity<ReaderView>,
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

        let reader = cx.new(|cx| ReaderView::new(engine, false, cx));
        let search_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Search mail  (/)")
                .clean_on_escape()
        });
        let subs = vec![cx.subscribe_in(
            &search_input,
            window,
            |this: &mut Self, input, ev: &InputEvent, window, cx| match ev {
                InputEvent::PressEnter { .. } => this.run_search(window, cx),
                InputEvent::Change
                    if input.read(cx).value().trim().is_empty() && this.search.is_some() =>
                {
                    this.clear_search(cx)
                }
                _ => {}
            },
        )];
        let focus = cx.focus_handle();
        focus.focus(window, cx);
        let mut this = Self {
            account,
            search_input,
            search_all: true,
            search: None,
            search_generation: 0,
            _subs: subs,
            focus,
            mailboxes: Vec::new(),
            mailbox: None,
            rows: Vec::new(),
            total: None,
            selected: None,
            list_scroll: UniformListScrollHandle::new(),
            loading_list: false,
            loading_more: false,
            reader,
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
            }
            Some(StoreEvent::MailboxesChanged) => self.refresh_mailboxes(cx),
            Some(StoreEvent::ListChanged(mb)) => {
                if self.mailbox.as_ref() == Some(&mb) {
                    self.reload_rows(cx);
                }
            }
            Some(StoreEvent::EmailsChanged(_)) => self.reload_rows(cx),
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
        self.show_selected(cx);
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
        let rows = match &self.search {
            Some(search) => {
                self.total = search.total;
                store.thread_rows(&search.ids, None)
            }
            None => {
                self.total = store.query(&mb).ok().flatten().and_then(|q| q.total);
                store.thread_list(&mb)
            }
        };
        match rows {
            Ok(rows) => self.rows = rows,
            Err(e) => self.status = Some(e.to_string().into()),
        }
        // Keep the same thread selected if it is still listed; otherwise keep the
        // index, so archiving moves on to the next conversation.
        if let Some(t) = selected_thread
            && let Some(ix) = self.selected
        {
            self.selected = match self.rows.iter().position(|r| r.thread_id == t) {
                Some(found) => Some(found),
                None if self.rows.is_empty() => None,
                None => Some(ix.min(self.rows.len() - 1)),
            };
            self.show_selected(cx);
        }
        cx.notify();
    }

    fn cached_len(&self) -> u64 {
        self.mailbox
            .as_ref()
            .and_then(|mb| self.engine().store().query(mb).ok().flatten())
            .map(|q| q.ids.len() as u64)
            .unwrap_or(0)
    }

    fn load_more(&mut self, cx: &mut Context<Self>) {
        if self.search.is_some() {
            return self.load_more_results(cx);
        }
        let position = self.cached_len();
        if self.loading_more || !self.total.is_some_and(|t| t > position) {
            return;
        }
        let Some(mb) = self.mailbox.clone() else {
            return;
        };
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

    /// Point the reader at the selected thread (or nothing).
    fn show_selected(&mut self, cx: &mut Context<Self>) {
        let thread = self.selected_row().map(|r| r.thread_id.clone());
        // In an all-mail search there is no "current mailbox" to archive from.
        let mailbox = match &self.search {
            Some(s) => s.scope.clone(),
            None => self.mailbox.clone(),
        };
        self.reader
            .update(cx, |r, cx| r.set_thread(thread, mailbox, cx));
    }

    fn select_row(&mut self, ix: usize, cx: &mut Context<Self>) {
        if ix >= self.rows.len() {
            return;
        }
        self.selected = Some(ix);
        self.list_scroll.scroll_to_item(ix, ScrollStrategy::Nearest);
        self.show_selected(cx);
        cx.notify();
    }

    fn report(&mut self, e: anyhow::Error, cx: &mut Context<Self>) {
        tracing::warn!("{e:#}");
        self.status = Some(format!("{e:#}").into());
        cx.notify();
    }

    // ----- search ---------------------------------------------------------

    fn run_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let query = self.search_input.read(cx).value().trim().to_string();
        if query.is_empty() {
            self.clear_search(cx);
            return;
        }
        let scope = if self.search_all {
            None
        } else {
            self.mailbox.clone()
        };
        self.search_generation += 1;
        let generation = self.search_generation;
        self.search = Some(Search {
            query: query.clone(),
            scope: scope.clone(),
            ids: Vec::new(),
            total: None,
            snippets: HashMap::new(),
            loading: true,
            generation,
        });
        self.selected = None;
        self.show_selected(cx);
        self.reload_rows(cx);
        self.list_scroll.scroll_to_item(0, ScrollStrategy::Top);
        // Hand the keyboard back to the list so j/k work on the results.
        self.focus.focus(window, cx);
        let engine = self.engine();
        cx.spawn(async move |this, cx| {
            let result =
                services::run(
                    async move { engine.search(&query, scope.as_ref(), 0, PAGE_SIZE).await },
                )
                .await;
            let _ = this.update(cx, |this, cx| {
                let Some(search) = this.search.as_mut().filter(|s| s.generation == generation)
                else {
                    return;
                };
                search.loading = false;
                match result {
                    Ok(page) => {
                        search.ids = page.ids;
                        search.total = page.total;
                        search.snippets = page.snippets;
                    }
                    Err(e) => this.report(e, cx),
                }
                this.reload_rows(cx);
            });
        })
        .detach();
        cx.notify();
    }

    fn load_more_results(&mut self, cx: &mut Context<Self>) {
        let Some(search) = self.search.as_mut() else {
            return;
        };
        let position = search.ids.len() as u64;
        if search.loading || !search.total.is_some_and(|t| t > position) {
            return;
        }
        search.loading = true;
        let (query, scope, generation) = (
            search.query.clone(),
            search.scope.clone(),
            search.generation,
        );
        let engine = self.engine();
        cx.spawn(async move |this, cx| {
            let result = services::run(async move {
                engine
                    .search(&query, scope.as_ref(), position, PAGE_SIZE)
                    .await
            })
            .await;
            let _ = this.update(cx, |this, cx| {
                let Some(search) = this.search.as_mut().filter(|s| s.generation == generation)
                else {
                    return;
                };
                search.loading = false;
                match result {
                    Ok(page) => {
                        search.ids.truncate(page.position as usize);
                        search.ids.extend(page.ids);
                        search.total = page.total;
                        search.snippets.extend(page.snippets);
                    }
                    Err(e) => this.report(e, cx),
                }
                this.reload_rows(cx);
            });
        })
        .detach();
    }

    fn clear_search(&mut self, cx: &mut Context<Self>) {
        if self.search.take().is_none() {
            return;
        }
        self.selected = None;
        self.show_selected(cx);
        self.reload_rows(cx);
        self.list_scroll.scroll_to_item(0, ScrollStrategy::Top);
        cx.notify();
    }

    /// Clear the search box and leave search mode.
    fn reset_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.search_input
            .update(cx, |i, cx| i.set_value("", window, cx));
        self.clear_search(cx);
    }

    fn focus_search(&mut self, _: &FocusSearch, window: &mut Window, cx: &mut Context<Self>) {
        self.search_input.update(cx, |i, cx| i.focus(window, cx));
    }

    // ----- actions --------------------------------------------------------

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

    fn pop_out(&mut self, ix: usize, cx: &mut Context<Self>) {
        if let Some(row) = self.rows.get(ix) {
            open_reader_window(
                self.engine(),
                row.thread_id.clone(),
                self.mailbox.clone(),
                cx,
            );
        }
    }

    /// Sign out: forget the token and settings, and delete everything cached
    /// locally for this account (database, generated browser pages).
    fn sign_out(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.push.abort();
        // Pop-out readers and source windows hold the database open; close them.
        let this_window = window.window_handle();
        for other in cx.windows() {
            if other != this_window {
                let _ = other.update(cx, |_, window, _| window.remove_window());
            }
        }
        let config = self.account.config.clone();
        let account = self.engine().account().to_string();
        let session_url = config.session_url.clone();
        cx.spawn(async move |this, cx| {
            let result = services::blocking(move || {
                config.forget()?;
                Config::remove_local_data(&account)?;
                browser_view::cleanup();
                Ok(())
            })
            .await;
            let error = result
                .err()
                .map(|e| format!("Signed out, but local data could not be fully removed: {e:#}"));
            let _ = this.update(cx, |_, cx| {
                cx.emit(MailEvent::SignedOut { session_url, error })
            });
        })
        .detach();
    }
}

// Thread actions go to the embedded reader.
macro_rules! forward {
    ($($action:ident => $method:ident),* $(,)?) => {
        impl MailView {
            $(
                fn $method(&mut self, a: &$action, window: &mut Window, cx: &mut Context<Self>) {
                    self.reader.update(cx, |r, cx| r.$method(a, window, cx));
                }
            )*
        }
    };
}

forward!(
    Archive => archive,
    Trash => trash,
    ToggleFlag => toggle_flag,
    ToggleRead => toggle_read,
    MoveTo => move_to,
    OpenInBrowser => open_newest_in_browser,
    OpenInNewWindow => open_in_new_window,
    ViewSource => view_source,
    ViewHeaders => view_headers,
);

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
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.reset_search(window, cx);
                    this.select_mailbox(id.clone(), cx)
                }))
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
                    .child("Rápido"),
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
                            .on_click(cx.listener(|this, _, window, cx| this.sign_out(window, cx))),
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
        // In search results, show the server's highlighted snippets when it has them.
        let snippet = self
            .search
            .as_ref()
            .and_then(|s| s.snippets.get(&row.email_id));
        let mark = HighlightStyle {
            background_color: Some(theme.warning.opacity(0.35)),
            font_weight: Some(FontWeight::BOLD),
            ..Default::default()
        };
        let highlighted = |snip: Option<&String>, fallback: &str| -> StyledText {
            match snip {
                Some(s) => {
                    let (text, ranges) = format::snippet_spans(s);
                    StyledText::new(text).with_highlights(ranges.into_iter().map(|r| (r, mark)))
                }
                None => StyledText::new(fallback.to_owned()),
            }
        };
        let subject_text = if row.subject.is_empty() {
            "(no subject)"
        } else {
            row.subject.as_str()
        };
        let subject = highlighted(snippet.and_then(|s| s.subject.as_ref()), subject_text);
        let preview = highlighted(snippet.and_then(|s| s.preview.as_ref()), &row.preview);
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
                            .when(row.has_attachment, |d| {
                                d.child(
                                    Icon::new(Lucide::Paperclip)
                                        .xsmall()
                                        .text_color(theme.muted_foreground),
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
                    .child(
                        div()
                            .truncate()
                            .text_sm()
                            .font_weight(weight)
                            .child(subject),
                    )
                    .child(
                        div()
                            .truncate()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(preview),
                    ),
            )
            .on_click(cx.listener(move |this, ev: &ClickEvent, _, cx| {
                this.select_row(ix, cx);
                if ev.click_count() >= 2 {
                    this.pop_out(ix, cx);
                }
            }))
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
                .child(match &self.search {
                    Some(s) if s.loading => "Searching…",
                    Some(_) => "Nothing found",
                    None if self.loading_list => "Loading…",
                    None => "No messages",
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
        let mailbox_name = self
            .mailbox
            .as_ref()
            .and_then(|id| self.mailboxes.iter().find(|m| &m.id == id))
            .map(|m| m.name.clone())
            .unwrap_or_default();
        let title = match &self.search {
            Some(s) => {
                let scope = if s.scope.is_some() {
                    mailbox_name.as_str()
                } else {
                    "all mail"
                };
                match s.total {
                    Some(n) => format!("{n} result{} in {scope}", if n == 1 { "" } else { "s" }),
                    None => format!("Searching {scope}…"),
                }
            }
            None => mailbox_name.clone(),
        };
        let busy = self.loading_more
            || self.loading_list
            || self.search.as_ref().is_some_and(|s| s.loading);
        let scope_label = if self.search_all {
            "All mail"
        } else {
            "This mailbox"
        };
        v_flex()
            .size_full()
            .child(
                h_flex()
                    .h(px(44.))
                    .flex_none()
                    .px_2()
                    .gap_1()
                    .border_b_1()
                    .border_color(theme.border)
                    .child(
                        div().flex_1().min_w_0().child(
                            Input::new(&self.search_input)
                                .id("search")
                                .prefix(
                                    Icon::new(IconName::Search)
                                        .small()
                                        .text_color(theme.muted_foreground),
                                )
                                .cleanable(true),
                        ),
                    )
                    .child(
                        Button::new("search-scope")
                            .ghost()
                            .small()
                            .label(scope_label)
                            .tooltip("Search all mail or only the selected mailbox")
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.search_all = !this.search_all;
                                if this.search.is_some() {
                                    this.run_search(window, cx);
                                }
                                cx.notify();
                            })),
                    ),
            )
            .child(
                h_flex()
                    .h(px(36.))
                    .flex_none()
                    .px_3()
                    .gap_2()
                    .border_b_1()
                    .border_color(theme.border)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(title),
                    )
                    .when(busy, |d| {
                        d.child(gpui_kit::component::spinner::Spinner::new().small())
                    })
                    .when(self.search.is_some(), |d| {
                        d.child(
                            Button::new("close-search")
                                .ghost()
                                .small()
                                .icon(IconName::Close)
                                .tooltip("Close search (Esc in the search box)")
                                .on_click(
                                    cx.listener(|this, _, window, cx| {
                                        this.reset_search(window, cx)
                                    }),
                                ),
                        )
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
            .on_action(cx.listener(Self::open_in_new_window))
            .on_action(cx.listener(Self::view_source))
            .on_action(cx.listener(Self::view_headers))
            .on_action(cx.listener(Self::refresh))
            .on_action(cx.listener(Self::focus_search))
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
                    .child(resizable_panel().child(self.reader.clone())),
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
