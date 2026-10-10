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

/// What clicking a notification does.
pub type OnClick = Box<dyn FnOnce() + Send>;

/// Shows a notification. On Linux, with `on_click`, it gets a Join button, and this waits until
/// it's clicked (the notification itself or Join both call `on_click`), dismissed, or expires: run
/// it on its own thread. Otherwise it returns once the notification is shown.
pub fn show(title: &str, body: &str, sound: bool, on_click: Option<OnClick>) -> Result<()> {
    let mut note = notify_rust::Notification::new();
    note.appname(APP_NAME).summary(title).body(body);
    platform(&mut note, sound, on_click.is_some());
    let handle = note.show().map_err(|e| anyhow!("{e}"))?;
    #[cfg(not(windows))]
    if let Some(on_click) = on_click {
        handle.wait_for_action(|action| {
            if action == "default" || action == "join" {
                on_click();
            }
        });
    }
    #[cfg(windows)]
    let _ = (handle, on_click);
    Ok(())
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
