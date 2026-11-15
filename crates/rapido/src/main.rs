mod actions;
mod assets;
mod body_text;
mod browser_view;
mod config;
mod format;
mod links;
mod mail;
mod root;
mod services;
mod setup;

use gpui_kit::component::{Theme, ThemeMode};
use gpui_kit::*;

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "rapido=info,rapido_store=info".into()),
        )
        .init();

    // Pages from a previous run may still be lying around.
    browser_view::cleanup();

    gpui_kit::application()
        .with_assets(assets::AppAssets)
        .run(|cx| {
            gpui_kit::init(cx);
            Theme::change(ThemeMode::Light, None, cx);
            Theme::sync_system_appearance(None, cx);
            actions::bind_keys(cx);
            cx.on_app_quit(|_| async { browser_view::cleanup() })
                .detach();

            let bounds = Bounds::centered(None, size(px(1200.), px(780.)), cx);
            let options = WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(TitlebarOptions {
                    title: Some("Rapido".into()),
                    ..Default::default()
                }),
                window_min_size: Some(size(px(720.), px(420.))),
                app_id: Some("cl.kddlb.rapido".into()),
                ..Default::default()
            };
            gpui_kit::open_window(options, cx, |window, cx| {
                cx.new(|cx| root::RapidoRoot::new(window, cx))
            })
            .expect("failed to open window");
        });
}
