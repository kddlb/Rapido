//! First-run / sign-in screen: session URL + API token.

use std::sync::Arc;

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::*;
use gpui_kit::prelude::*;
use gpui_kit::*;
use jmap_http::FASTMAIL_SESSION_URL;

use crate::services::{self, Account};

pub enum SetupEvent {
    SignedIn(Arc<Account>),
}

pub struct SetupView {
    url: Entity<InputState>,
    token: Entity<InputState>,
    busy: bool,
    error: Option<SharedString>,
    _subs: Vec<Subscription>,
}

impl EventEmitter<SetupEvent> for SetupView {}

impl SetupView {
    pub fn new(
        session_url: Option<String>,
        error: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let url = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(FASTMAIL_SESSION_URL)
                .default_value(session_url.unwrap_or_else(|| FASTMAIL_SESSION_URL.to_owned()))
        });
        let token = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("fmu1-…")
                .masked(true)
        });
        let subs = vec![cx.subscribe_in(
            &token,
            window,
            |this: &mut Self, _, ev: &InputEvent, window, cx| {
                if let InputEvent::PressEnter { .. } = ev {
                    this.sign_in(window, cx);
                }
            },
        )];
        token.update(cx, |t, cx| t.focus(window, cx));
        Self {
            url,
            token,
            busy: false,
            error: error.map(Into::into),
            _subs: subs,
        }
    }

    fn sign_in(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let url = self.url.read(cx).value().trim().to_string();
        let url = if url.is_empty() {
            FASTMAIL_SESSION_URL.to_owned()
        } else {
            url
        };
        let token = self.token.read(cx).value().trim().to_string();
        if token.is_empty() {
            self.error = Some("Paste an API token first.".into());
            cx.notify();
            return;
        }
        self.busy = true;
        self.error = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = services::spawn(async move {
                let account = services::sign_in(url, token.clone()).await?;
                account.config.save()?;
                account.config.store_token(&token)?;
                anyhow::Ok(account)
            })
            .await
            .and_then(|r| r);
            let _ = this.update(cx, |this, cx| {
                this.busy = false;
                match result {
                    Ok(account) => cx.emit(SetupEvent::SignedIn(Arc::new(account))),
                    Err(e) => this.error = Some(format!("{e:#}").into()),
                }
                cx.notify();
            });
        })
        .detach();
    }
}

impl Render for SetupView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .bg(theme.background)
            .child(
                v_flex()
                    .w(px(440.))
                    .gap_3()
                    .p_6()
                    .rounded(theme.radius_lg)
                    .border_1()
                    .border_color(theme.border)
                    .child(
                        div()
                            .text_xl()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("Sign in to Rapido"),
                    )
                    .child(div().text_sm().text_color(theme.muted_foreground).child(
                        "Rapido talks JMAP. For Fastmail, create an API token under \
                             Settings → Privacy & Security → Manage API tokens, with \
                             access to Email.",
                    ))
                    .child(div().text_sm().child("Session URL"))
                    .child(Input::new(&self.url).id("session-url"))
                    .child(div().text_sm().child("API token"))
                    .child(Input::new(&self.token).id("token").mask_toggle())
                    .children(
                        self.error
                            .clone()
                            .map(|e| div().text_sm().text_color(theme.danger).child(e)),
                    )
                    .child(
                        h_flex().justify_end().child(
                            Button::new("sign-in")
                                .primary()
                                .label(if self.busy {
                                    "Signing in…"
                                } else {
                                    "Sign in"
                                })
                                .loading(self.busy)
                                .on_click(
                                    cx.listener(|this, _, window, cx| this.sign_in(window, cx)),
                                ),
                        ),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child("The token is stored in your system keyring."),
                    ),
            )
    }
}
