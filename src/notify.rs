//! Desktop notifications: through the desktop's notification service on Linux (D-Bus), and as
//! toasts on Windows.
//!
//! On Windows they're only informational. A toast names the program that sent it by its
//! AppUserModelID, which Windows only accepts from programs registered with a Start menu
//! shortcut; the browser is unpacked from a zip and has none, so its toasts go out under
//! PowerShell's id (the default), show "Windows PowerShell" as the sender, and clicks on them
//! aren't reliably delivered back.

use anyhow::{Result, anyhow};

/// The name notifications are sent under, where the desktop shows one.
const APP_NAME: &str = "DayZ Server Browser";

/// How long a notification with a Join button waits for a click. After that it's taken down:
/// room on a server rarely lasts that long, and desktops that keep notifications until they're
/// dismissed would otherwise keep a thread and a D-Bus connection waiting for each.
#[cfg(not(windows))]
const WAIT_FOR_CLICK: std::time::Duration = std::time::Duration::from_secs(5 * 60);

/// What clicking a notification does.
pub type OnClick = Box<dyn FnOnce() + Send>;

/// Shows a notification, with plain text in the title and body. On Linux, with `on_click`, it
/// gets a Join button, and this waits until it's clicked (the notification itself or Join both
/// call `on_click`), dismissed, or a few minutes have passed: run it on its own thread.
/// Otherwise it returns once the notification is shown.
pub fn show(title: &str, body: &str, sound: bool, on_click: Option<OnClick>) -> Result<()> {
    let mut note = notify_rust::Notification::new();
    // Desktops read markup in the body (not the title) on Linux; toasts are built as XML
    // documents, which takes care of it on Windows.
    #[cfg(not(windows))]
    let body = escape_markup(body);
    #[cfg(not(windows))]
    let body = body.as_str();
    note.appname(APP_NAME).summary(title).body(body);
    platform(&mut note, sound, on_click.is_some());
    let handle = note.show().map_err(|e| anyhow!("{e}"))?;
    #[cfg(not(windows))]
    if let Some(on_click) = on_click {
        wait_for_click(handle, on_click, WAIT_FOR_CLICK);
    }
    #[cfg(windows)]
    let _ = (handle, on_click);
    Ok(())
}

/// Waits for a click on the notification, for `wait` at most; then closes it.
#[cfg(not(windows))]
fn wait_for_click(
    handle: notify_rust::NotificationHandle,
    on_click: OnClick,
    wait: std::time::Duration,
) {
    use futures_lite::future;
    use notify_rust::NotificationResponse;
    let mut clicked = false;
    let answered = future::block_on(future::or(
        async {
            handle
                .wait_for_action_async(|response| {
                    clicked = match response {
                        NotificationResponse::Default => true,
                        NotificationResponse::Action(action) => action == "join",
                        _ => false,
                    };
                })
                .await;
            true
        },
        async {
            async_io::Timer::after(wait).await;
            false
        },
    ));
    if !answered {
        handle.close();
    }
    if clicked {
        on_click();
    }
}

/// Escapes text for a notification body, which desktops on Linux read as markup: a server's
/// `<a href=…>` mustn't show as a link, nor a stray `&` break the rest.
#[cfg(not(windows))]
fn escape_markup(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

/// Text from a server (its name, its map) fit for a notification: control characters dropped,
/// spaces trimmed, and cut to `max` characters with an ellipsis.
pub fn server_text(text: &str, max: usize) -> String {
    let text = crate::server::clean_name(text);
    if text.chars().count() <= max {
        return text;
    }
    let mut cut: String = text.chars().take(max.saturating_sub(1)).collect();
    cut.truncate(cut.trim_end().len());
    cut.push('…');
    cut
}

#[cfg(not(windows))]
fn platform(note: &mut notify_rust::Notification, sound: bool, actions: bool) {
    if sound {
        // The freedesktop sound theme's name for an incoming message.
        note.sound_name("message-new-instant");
    } else {
        note.hint(notify_rust::Hint::SuppressSound(true));
    }
    if actions {
        // "default" is a click on the notification itself; desktops don't show it as a button.
        note.action("default", "Open").action("join", "Join");
    }
}

#[cfg(windows)]
fn platform(note: &mut notify_rust::Notification, sound: bool, _actions: bool) {
    // Toasts without a sound are silent.
    if sound {
        note.sound_name("Default");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(not(windows))]
    #[test]
    fn escapes_markup() {
        assert_eq!(
            escape_markup(r#"<a href="x">Tom & Jerry's</a>"#),
            "&lt;a href=&quot;x&quot;&gt;Tom &amp; Jerry&#39;s&lt;/a&gt;"
        );
        assert_eq!(
            escape_markup("57/60 players · Chernarus"),
            "57/60 players · Chernarus"
        );
    }

    #[test]
    fn cleans_server_text() {
        assert_eq!(server_text(" Best\u{7}\nServer ", 100), "BestServer");
        assert_eq!(server_text("abcdef", 6), "abcdef");
        assert_eq!(server_text("abcdefg", 6), "abcde…");
        assert_eq!(server_text("abcd efg", 6), "abcd…");
        assert_eq!(server_text("ééééééé", 3), "éé…");
    }
}
