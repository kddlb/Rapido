//! Top-level view: switches between loading, sign-in and the mail window.

use std::sync::Arc;

use gpui_kit::component::*;
use gpui_kit::prelude::*;
use gpui_kit::*;

use crate::config::Config;
use crate::mail::{MailEvent, MailView};
use crate::services::{self, Account};
use crate::setup::{SetupEvent, SetupView};

enum Screen {
    Loading,
    Setup(Entity<SetupView>),
    Mail(Entity<MailView>),
}

pub struct RapidoRoot {
    screen: Screen,
    _subs: Vec<Subscription>,
}

impl RapidoRoot {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let appearance = cx.observe_window_appearance(window, |_, window, cx| {
            Theme::sync_system_appearance(Some(window), cx);
        });
        let mut this = Self {
            screen: Screen::Loading,
            _subs: vec![appearance],
        };
        match Config::load() {
            Ok(Some(config)) => this.resume(config, window, cx),
            Ok(None) => this.show_setup(None, None, window, cx),
            Err(e) => this.show_setup(None, Some(format!("{e:#}")), window, cx),
        }
        this
    }

    fn resume(&mut self, config: Config, window: &mut Window, cx: &mut Context<Self>) {
        let url = config.session_url.clone();
        cx.spawn_in(window, async move |this, cx| {
            let result = services::spawn(services::resume(config))
                .await
                .and_then(|r| r);
            let _ = this.update_in(cx, |this, window, cx| match result {
                Ok(account) => this.show_mail(Arc::new(account), window, cx),
                Err(e) => this.show_setup(Some(url), Some(format!("{e:#}")), window, cx),
            });
        })
        .detach();
    }

    fn show_setup(
        &mut self,
        url: Option<String>,
        error: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let view = cx.new(|cx| SetupView::new(url, error, window, cx));
        let sub = cx.subscribe_in(
            &view,
            window,
            |this, _, ev: &SetupEvent, window, cx| match ev {
                SetupEvent::SignedIn(account) => this.show_mail(account.clone(), window, cx),
            },
        );
        self._subs.truncate(1);
        self._subs.push(sub);
        self.screen = Screen::Setup(view);
        cx.notify();
    }

    fn show_mail(&mut self, account: Arc<Account>, window: &mut Window, cx: &mut Context<Self>) {
        window.set_window_title(&format!("Rápido — {}", account.username()));
        let view = cx.new(|cx| MailView::new(account, window, cx));
        let sub = cx.subscribe_in(
            &view,
            window,
            |this, _, ev: &MailEvent, window, cx| match ev {
                MailEvent::SignedOut { session_url } => {
                    window.set_window_title("Rápido");
                    this.show_setup(Some(session_url.clone()), None, window, cx)
                }
            },
        );
        self._subs.truncate(1);
        self._subs.push(sub);
        self.screen = Screen::Mail(view);
        cx.notify();
    }
}

impl Render for RapidoRoot {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let base = div()
            .size_full()
            .bg(theme.background)
            .text_color(theme.foreground);
        match &self.screen {
            Screen::Loading => base.child(
                v_flex()
                    .size_full()
                    .items_center()
                    .justify_center()
                    .gap_2()
                    .child(gpui_kit::component::spinner::Spinner::new())
                    .child(
                        div()
                            .text_sm()
                            .text_color(theme.muted_foreground)
                            .child("Connecting…"),
                    ),
            ),
            Screen::Setup(v) => base.child(v.clone()),
            Screen::Mail(v) => base.child(v.clone()),
        }
    }
}
