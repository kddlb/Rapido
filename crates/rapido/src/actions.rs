//! Keyboard actions for the mail window and pop-out reader windows.

use gpui_kit::{App, KeyBinding, actions};

pub const MAIL_CONTEXT: &str = "Mail";
pub const READER_CONTEXT: &str = "Reader";
pub const SOURCE_CONTEXT: &str = "Source";

/// Single-key shortcuts must not fire while typing in a text field.
const MAIL_KEYS: &str = "Mail && !Input";

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
        ViewSource,
        ViewHeaders,
        CloseWindow,
        Refresh,
        FocusSearch,
    ]
);

pub fn bind_keys(cx: &mut App) {
    let keys = Some(MAIL_KEYS);
    let mail = Some(MAIL_CONTEXT);
    cx.bind_keys([
        KeyBinding::new("j", NextThread, keys),
        KeyBinding::new("down", NextThread, keys),
        KeyBinding::new("k", PrevThread, keys),
        KeyBinding::new("up", PrevThread, keys),
        KeyBinding::new("enter", OpenInNewWindow, keys),
        KeyBinding::new("/", FocusSearch, keys),
        KeyBinding::new("ctrl-f", FocusSearch, mail),
        KeyBinding::new("ctrl-r", Refresh, mail),
    ]);
    // Thread actions work in the main window and in pop-out readers.
    for ctx in [MAIL_KEYS, READER_CONTEXT] {
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
    for ctx in [MAIL_CONTEXT, READER_CONTEXT] {
        let ctx = Some(ctx);
        cx.bind_keys([
            KeyBinding::new("ctrl-u", ViewSource, ctx),
            KeyBinding::new("ctrl-shift-h", ViewHeaders, ctx),
        ]);
    }
    for ctx in [READER_CONTEXT, SOURCE_CONTEXT] {
        let ctx = Some(ctx);
        cx.bind_keys([
            KeyBinding::new("ctrl-w", CloseWindow, ctx),
            KeyBinding::new("escape", CloseWindow, ctx),
        ]);
    }
}
