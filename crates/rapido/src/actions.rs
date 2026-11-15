//! Keyboard actions for the mail window and pop-out reader windows.

use gpui_kit::{App, KeyBinding, actions};

pub const MAIL_CONTEXT: &str = "Mail";
pub const READER_CONTEXT: &str = "Reader";

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
        OpenInNewWindow,
        CloseWindow,
        Refresh,
    ]
);

pub fn bind_keys(cx: &mut App) {
    let mail = Some(MAIL_CONTEXT);
    cx.bind_keys([
        KeyBinding::new("j", NextThread, mail),
        KeyBinding::new("down", NextThread, mail),
        KeyBinding::new("k", PrevThread, mail),
        KeyBinding::new("up", PrevThread, mail),
        KeyBinding::new("enter", OpenInNewWindow, mail),
        KeyBinding::new("ctrl-r", Refresh, mail),
    ]);
    // Thread actions work in the main window and in pop-out readers.
    for ctx in [MAIL_CONTEXT, READER_CONTEXT] {
        let ctx = Some(ctx);
        cx.bind_keys([
            KeyBinding::new("e", Archive, ctx),
            KeyBinding::new("#", Trash, ctx),
            KeyBinding::new("shift-#", Trash, ctx),
            KeyBinding::new("delete", Trash, ctx),
            KeyBinding::new("s", ToggleFlag, ctx),
            KeyBinding::new("u", ToggleRead, ctx),
            KeyBinding::new("v", MoveTo, ctx),
            KeyBinding::new("o", OpenInBrowser, ctx),
        ]);
    }
    let reader = Some(READER_CONTEXT);
    cx.bind_keys([
        KeyBinding::new("ctrl-w", CloseWindow, reader),
        KeyBinding::new("escape", CloseWindow, reader),
    ]);
}
