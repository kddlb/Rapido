//! Windows showing a message's full header list or its raw RFC 5322 source,
//! in a read-only editor (selectable, searchable, efficient on large text).

use std::sync::Arc;

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Editor, EditorState};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::scroll::ScrollableElement;
use gpui_kit::component::text::TextView;
use gpui_kit::component::*;
use gpui_kit::prelude::*;
use gpui_kit::*;
use jmap_proto::Id;
use jmap_proto::mail::EmailHeader;
use rapido_store::SyncEngine;

use crate::actions::*;
use crate::reader::{confirm_open_link, link_tooltip};
use crate::{attachments, header_fields, links, services};

/// Raw sources above this are shown truncated (the full source can be saved).
const MAX_DISPLAY_BYTES: usize = 4 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceKind {
    Headers,
    Raw,
}

impl SourceKind {
    fn label(self) -> &'static str {
        match self {
            Self::Headers => "Headers",
            Self::Raw => "Source",
        }
    }
}

/// All header fields, in order, exactly as received (`Name:value`, folded
/// lines kept), with line endings normalized.
pub fn format_headers(headers: &[EmailHeader]) -> String {
    let mut out = String::new();
    for h in headers {
        out.push_str(&h.name);
        out.push(':');
        out.push_str(&h.value.replace("\r\n", "\n").replace('\r', "\n"));
        out.push('\n');
    }
    out
}

/// Text for the raw view: lossy UTF-8, normalized line endings, capped.
/// Returns the text and whether it was truncated.
pub fn raw_display(bytes: &[u8]) -> (String, bool) {
    let truncated = bytes.len() > MAX_DISPLAY_BYTES;
    let shown = &bytes[..bytes.len().min(MAX_DISPLAY_BYTES)];
    let text = String::from_utf8_lossy(shown).replace("\r\n", "\n");
    (text, truncated)
}

/// Tabs of the headers window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HeaderTab {
    Fields,
    Raw,
}

pub struct SourceView {
    kind: SourceKind,
    tab: HeaderTab,
    /// Structured header table (HTML for the text view), headers window only.
    table: Option<SharedString>,
    subject: String,
    editor: Entity<EditorState>,
    raw: Option<Arc<Vec<u8>>>,
    text: Option<SharedString>,
    loading: bool,
    truncated: bool,
    error: Option<SharedString>,
    focus: FocusHandle,
}

impl Focusable for SourceView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl SourceView {
    fn new(
        engine: Arc<SyncEngine>,
        email: Id,
        subject: String,
        kind: SourceKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let editor = cx.new(|cx| {
            EditorState::new(window, cx)
                .line_number(kind == SourceKind::Raw)
                .searchable(true)
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = match kind {
                SourceKind::Headers => services::run(async move { engine.headers(&email).await })
                    .await
                    .map(|h| {
                        (
                            format_headers(&h),
                            None,
                            false,
                            Some(header_fields::table_html(&h)),
                        )
                    }),
                SourceKind::Raw => services::run(async move { engine.raw_message(&email).await })
                    .await
                    .map(|bytes| {
                        let (text, truncated) = raw_display(&bytes);
                        (text, Some(Arc::new(bytes.to_vec())), truncated, None)
                    }),
            };
            let _ = this.update_in(cx, |this, window, cx| {
                this.loading = false;
                match result {
                    Ok((text, raw, truncated, table)) => {
                        this.table = table.map(Into::into);
                        this.editor
                            .update(cx, |e, cx| e.set_value(text.clone(), window, cx));
                        this.text = Some(text.into());
                        this.raw = raw;
                        this.truncated = truncated;
                    }
                    Err(e) => this.error = Some(format!("{e:#}").into()),
                }
                cx.notify();
            });
        })
        .detach();
        Self {
            kind,
            tab: HeaderTab::Fields,
            table: None,
            subject,
            editor,
            raw: None,
            text: None,
            loading: true,
            truncated: false,
            error: None,
            focus: cx.focus_handle(),
        }
    }

    fn copy_all(&mut self, cx: &mut Context<Self>) {
        if let Some(t) = &self.text {
            cx.write_to_clipboard(ClipboardItem::new_string(t.to_string()));
        }
    }

    fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(raw) = self.raw.clone() else { return };
        let name = attachments::file_name(&jmap_proto::mail::EmailBodyPart {
            name: Some(format!(
                "{}.eml",
                if self.subject.is_empty() {
                    "message"
                } else {
                    &self.subject
                }
            )),
            ..Default::default()
        });
        let chosen = cx.prompt_for_new_path(&attachments::default_dir(), Some(&name));
        cx.spawn_in(window, async move |_, cx| {
            let Ok(Ok(Some(path))) = chosen.await else {
                return;
            };
            let target = path.clone();
            let result =
                services::blocking(move || Ok(std::fs::write(&target, raw.as_slice())?)).await;
            let _ = cx.update(|window, cx| match result {
                Ok(()) => window.push_notification(
                    Notification::success(format!("Saved {}", path.display())),
                    cx,
                ),
                Err(e) => window
                    .push_notification(Notification::error(format!("Could not save: {e:#}")), cx),
            });
        })
        .detach();
    }

    fn close_window(&mut self, _: &CloseWindow, window: &mut Window, _: &mut Context<Self>) {
        window.remove_window();
    }
}

impl Render for SourceView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let body: AnyElement = if let Some(e) = &self.error {
            div()
                .p_4()
                .text_sm()
                .text_color(theme.danger)
                .child(e.clone())
                .into_any_element()
        } else if self.loading {
            v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .child(gpui_kit::component::spinner::Spinner::new())
                .into_any_element()
        } else if self.kind == SourceKind::Headers && self.tab == HeaderTab::Fields {
            div()
                .id("header-fields")
                .size_full()
                .overflow_y_scrollbar()
                .p_3()
                .child(
                    TextView::html("header-table", self.table.clone().unwrap_or_default())
                        .selectable(true)
                        .on_link_click(|url, _, window, cx| {
                            confirm_open_link(links::inspect(url), window, cx)
                        })
                        .link_tooltip(link_tooltip)
                        .text_sm(),
                )
                .into_any_element()
        } else {
            Editor::new(&self.editor)
                .readonly(true)
                .bordered(false)
                .h(relative(1.))
                .font_family(theme.mono_font_family.clone())
                .text_size(theme.mono_font_size)
                .into_any_element()
        };
        v_flex()
            .size_full()
            .bg(theme.background)
            .text_color(theme.foreground)
            .key_context(SOURCE_CONTEXT)
            .track_focus(&self.focus)
            .on_action(cx.listener(Self::close_window))
            .child(
                h_flex()
                    .h(px(44.))
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
                            .child(format!("{} — {}", self.kind.label(), self.subject)),
                    )
                    .when(self.kind == SourceKind::Headers, |d| {
                        let tab = self.tab;
                        d.child(
                            Button::new("tab-fields")
                                .small()
                                .label("Fields")
                                .selected(tab == HeaderTab::Fields)
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.tab = HeaderTab::Fields;
                                    cx.notify();
                                })),
                        )
                        .child(
                            Button::new("tab-raw")
                                .small()
                                .label("Raw")
                                .selected(tab == HeaderTab::Raw)
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.tab = HeaderTab::Raw;
                                    cx.notify();
                                })),
                        )
                    })
                    .child(
                        Button::new("copy-all")
                            .ghost()
                            .small()
                            .icon(IconName::Copy)
                            .label("Copy all")
                            .disabled(self.text.is_none())
                            .on_click(cx.listener(|this, _, _, cx| this.copy_all(cx))),
                    )
                    .when(self.kind == SourceKind::Raw, |d| {
                        d.child(
                            Button::new("save-eml")
                                .ghost()
                                .small()
                                .label("Save as .eml…")
                                .disabled(self.raw.is_none())
                                .on_click(cx.listener(|this, _, window, cx| this.save(window, cx))),
                        )
                    }),
            )
            .when(self.truncated, |d| {
                d.child(
                    div()
                        .px_3()
                        .py_1()
                        .text_xs()
                        .bg(theme.muted)
                        .text_color(theme.warning)
                        .child("This message is large; only the first 4 MB is shown. Save it as .eml for the rest."),
                )
            })
            .child(div().flex_1().min_h_0().child(body))
    }
}

/// Open a header or source window for one email.
pub fn open_source_window(
    engine: Arc<SyncEngine>,
    email: Id,
    subject: String,
    kind: SourceKind,
    cx: &mut App,
) {
    let bounds = Bounds::centered(None, size(px(860.), px(680.)), cx);
    let title = format!(
        "{} — {} — Rápido",
        kind.label(),
        if subject.is_empty() {
            "(no subject)"
        } else {
            &subject
        }
    );
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        titlebar: Some(TitlebarOptions {
            title: Some(title.into()),
            ..Default::default()
        }),
        window_min_size: Some(size(px(420.), px(300.))),
        app_id: Some("cl.kddlb.rapido".into()),
        ..Default::default()
    };
    let result = gpui_kit::open_window(options, cx, move |window, cx| {
        cx.new(|cx| {
            let view = SourceView::new(engine, email, subject, kind, window, cx);
            view.focus.focus(window, cx);
            view
        })
    });
    if let Err(e) = result {
        tracing::warn!("could not open {} window: {e:#}", kind.label());
    }
}

#[cfg(test)]
mod tests {
    use super::{MAX_DISPLAY_BYTES, format_headers, raw_display};
    use jmap_proto::mail::EmailHeader;

    #[test]
    fn headers_keep_order_and_folding() {
        let h = vec![
            EmailHeader {
                name: "Received".into(),
                value: " from a\r\n\tby b".into(),
            },
            EmailHeader {
                name: "X-Odd".into(),
                value: "novalue-space".into(),
            },
        ];
        assert_eq!(
            format_headers(&h),
            "Received: from a\n\tby b\nX-Odd:novalue-space\n"
        );
    }

    #[test]
    fn raw_is_lossy_and_capped() {
        let (t, cut) = raw_display(b"A: b\r\n\r\n\xff body");
        assert_eq!(t, "A: b\n\n\u{fffd} body");
        assert!(!cut);
        let big = vec![b'x'; MAX_DISPLAY_BYTES + 10];
        let (t, cut) = raw_display(&big);
        assert!(cut);
        assert_eq!(t.len(), MAX_DISPLAY_BYTES);
    }
}
