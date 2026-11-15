//! Keyboard actions for the mail window.

use gpui_kit::{App, KeyBinding, actions};

pub const MAIL_CONTEXT: &str = "Mail";

actions!(
    rapido,
    [
        NextThread,
        PrevThread,
        Archive,
        Trash,
        ToggleFlag,
        ToggleRead,
        MoveTo,
        OpenInBrowser,
        Refresh,
    ]
);

pub fn bind_keys(cx: &mut App) {
    let ctx = Some(MAIL_CONTEXT);
    cx.bind_keys([
        KeyBinding::new("j", NextThread, ctx),
        KeyBinding::new("down", NextThread, ctx),
        KeyBinding::new("k", PrevThread, ctx),
        KeyBinding::new("up", PrevThread, ctx),
        KeyBinding::new("e", Archive, ctx),
        KeyBinding::new("#", Trash, ctx),
        KeyBinding::new("shift-#", Trash, ctx),
        KeyBinding::new("delete", Trash, ctx),
        KeyBinding::new("s", ToggleFlag, ctx),
        KeyBinding::new("u", ToggleRead, ctx),
        KeyBinding::new("v", MoveTo, ctx),
        KeyBinding::new("o", OpenInBrowser, ctx),
        KeyBinding::new("ctrl-r", Refresh, ctx),
    ]);
}
