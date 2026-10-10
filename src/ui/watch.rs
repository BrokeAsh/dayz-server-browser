//! The notification watch list: "Notify when there's room" in a server's right-click menu and
//! Notify in its details, the status bar's button with the count, the small window listing the
//! watched servers and how each stands, the Notifications settings, and what clicking a
//! notification does. The checking and the notifying are in `crate::watch`.

use eframe::egui::{self, Align, Color32, Layout, RichText};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use super::{App, GOOD, Msg, WARN};
use crate::address;
use crate::notify;
use crate::server::map_title;
use crate::settings::Watched;
use crate::watch::{self, Out, Status, Watcher};

/// The watch list window's id.
const WINDOW: &str = "watch-window";

pub struct WatchList {
    watcher: Watcher,
    /// Each watched server's latest status, and when it was checked; none until its first check.
    status: HashMap<String, (Status, Instant)>,
    /// The window listing them is open.
    window: bool,
    /// When the toast showing was put up for notifications that couldn't be shown.
    not_shown: Option<Instant>,
    /// A test notification is being sent (it can take a while with no notification service).
    testing: Arc<AtomicBool>,
}

impl WatchList {
    pub fn start(ctx: &egui::Context, tx: &mpsc::Sender<Msg>, sound: bool) -> Self {
        let (ctx, tx) = (ctx.clone(), tx.clone());
        let watcher = Watcher::spawn(sound, move |out| {
            let _ = tx.send(Msg::Watch(out));
            ctx.request_repaint();
        });
        Self {
            watcher,
            status: HashMap::new(),
            window: false,
            not_shown: None,
            testing: Arc::default(),
        }
    }
}

impl App {
    /// Starts checking the servers watched from before (at start).
    pub(super) fn resume_watching(&mut self) {
        // Drop blanks and repeats a hand-edited or damaged file might have.
        let mut seen = std::collections::HashSet::new();
        self.state
            .watched
            .retain(|w| !w.addr.is_empty() && seen.insert(w.addr.clone()));
        let addrs = self.state.watched.iter().map(|w| w.addr.clone()).collect();
        self.watch_list.watcher.resume(addrs);
    }

    /// Adds a server to the watch list, or takes it off.
    pub(super) fn toggle_watch(&mut self, addr: &str) {
        if let Some(i) = self.state.watched.iter().position(|w| w.addr == addr) {
            self.state.watched.remove(i);
            self.watch_list.watcher.unwatch(addr);
            self.watch_list.status.remove(addr);
        } else {
            let listed = self.index.get(addr).map(|&i| &self.servers[i]);
            let info = self.live.get(addr).and_then(|l| l.info.as_ref());
            let (name, map) = match (listed, info) {
                (Some(s), _) => (s.name.clone(), s.map.clone()),
                (None, Some(info)) => (info.name.clone(), info.map.clone()),
                (None, None) => (String::new(), String::new()),
            };
            self.state.watched.push(Watched {
                addr: addr.to_string(),
                name,
                map,
            });
            self.watch_list.watcher.watch(addr);
        }
        self.state_changed();
    }

    pub(super) fn watch_msg(&mut self, out: Out) {
        match out {
            Out::Checked {
                pong,
                status,
                added,
            } => {
                // Ignored for a server taken off the list while it was being asked.
                if let Some(i) = self.state.watched.iter().position(|w| w.addr == pong.addr) {
                    self.watch_list
                        .status
                        .insert(pong.addr.clone(), (status, Instant::now()));
                    // Keep its name for when it isn't in the list or isn't answering.
                    let w = &mut self.state.watched[i];
                    let renamed = pong.info.as_ref().is_some_and(|info| {
                        !info.name.is_empty() && (w.name != info.name || w.map != info.map)
                    });
                    if renamed && let Some(info) = &pong.info {
                        w.name = info.name.clone();
                        w.map = info.map.clone();
                    }
                    let name = shown_name(w);
                    if renamed {
                        self.state_changed();
                    }
                    if added {
                        self.toast(match status {
                            Status::Room { players, slots } => {
                                format!("{name} has room now ({players}/{slots}, no queue)")
                            }
                            Status::Silent => format!(
                                "{name} isn't answering; you'll be notified when it has room"
                            ),
                            _ => format!("You'll be notified when {name} has room"),
                        });
                    }
                }
                // The list shows the fresh numbers too.
                self.handle(Msg::Pong(pong));
            }
            Out::Clicked(addr) => self.join_watched(&addr),
            Out::NotShown(text) => {
                // Several at once (they fail together when there's no notification service)
                // all show, rather than each replacing the last.
                let text = match (&self.toast, self.watch_list.not_shown) {
                    (Some((shown, at)), Some(put_up))
                        if *at == put_up && at.elapsed() < Duration::from_secs(4) =>
                    {
                        format!("{shown}\n{text}")
                    }
                    _ => text,
                };
                self.toast(text);
                self.watch_list.not_shown = self.toast.as_ref().map(|(_, at)| *at);
            }
        }
    }

    /// A notification was clicked: bring the window forward and open the join window.
    fn join_watched(&mut self, addr: &str) {
        let ctx = &self.ctx;
        ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        // Where the desktop doesn't let a program take the focus (Wayland), it can still ask
        // for attention.
        ctx.send_viewport_cmd(egui::ViewportCommand::RequestUserAttention(
            egui::UserAttentionType::Informational,
        ));
        if self.ensure_listed(addr) {
            self.select(addr, true);
            self.open_join(addr);
        } else {
            // The details panel offers to ask it.
            self.select(addr, true);
        }
    }

    /// Makes sure a watched server is in the list, adding it from its latest answer if the list
    /// lacks it (a refresh dropped it, or it was added by address). False if it has never
    /// answered.
    fn ensure_listed(&mut self, addr: &str) -> bool {
        if self.index.contains_key(addr) {
            return true;
        }
        let Some(info) = self.live.get(addr).and_then(|l| l.info.clone()) else {
            return false;
        };
        let Ok(target) = addr.parse::<SocketAddr>() else {
            return false;
        };
        let found = address::found(target.ip(), target.port(), info, Duration::ZERO);
        self.added.retain(|s| s.addr() != addr);
        self.added.push(found.server);
        self.reapply_added();
        self.index.contains_key(addr)
    }

    /// The Notify toggle in the details panel.
    pub(super) fn watch_toggle(&mut self, ui: &mut egui::Ui, addr: &str) {
        let watched = self.state.is_watched(addr);
        let (label, hint) = if watched {
            (
                "🔔 Notifying",
                "On your watch list: you'll be notified when it has room. Click to stop.",
            )
        } else {
            (
                "🔔 Notify",
                "Put it on your watch list, to be notified when there's room",
            )
        };
        if ui
            .selectable_label(watched, label)
            .on_hover_text(hint)
            .clicked()
        {
            self.toggle_watch(addr);
        }
    }

    /// The status bar's button for the watch list: how many are watched, and how many have room.
    /// Nothing while the list is empty.
    pub(super) fn watch_button(&mut self, ui: &mut egui::Ui) {
        let watched = self.state.watched.len();
        if watched == 0 {
            return;
        }
        let room = self
            .state
            .watched
            .iter()
            .filter(|w| self.status_of(&w.addr).is_some_and(|(s, _)| s.room()))
            .count();
        let text = match room {
            0 => RichText::new(format!("🔔 {watched} watched")),
            1 => RichText::new(format!("🔔 1 of {watched} watched has room")).color(GOOD),
            _ => RichText::new(format!("🔔 {room} of {watched} watched have room")).color(GOOD),
        };
        ui.separator();
        if ui
            .selectable_label(self.watch_list.window, text)
            .on_hover_text("Your watch list: servers you'll be notified about when they have room")
            .clicked()
        {
            self.watch_list.window = !self.watch_list.window;
        }
    }

    fn status_of(&self, addr: &str) -> Option<(Status, Instant)> {
        self.watch_list.status.get(addr).copied()
    }

    /// The small window listing the watched servers.
    pub(super) fn watch_window(&mut self, ctx: &egui::Context) {
        if !self.watch_list.window {
            return;
        }
        let mut open = true;
        let mut remove = None;
        let mut pick = None;
        let mut join = None;
        let id = egui::Id::new(WINDOW);
        let corner = ctx.content_rect().right_bottom() - egui::vec2(12.0, 36.0);
        egui::Window::new("Watch list")
            .id(id)
            .collapsible(false)
            .resizable(false)
            .default_width(300.0)
            .pivot(egui::Align2::RIGHT_BOTTOM)
            .default_pos(corner)
            .open(&mut open)
            .show(ctx, |ui| {
                if self.state.watched.is_empty() {
                    ui.label(
                        RichText::new(
                            "Right-click a server in the list and pick \"Notify when there's \
                             room\".",
                        )
                        .weak(),
                    );
                }
                egui::ScrollArea::vertical()
                    .max_height(300.0)
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        for (i, w) in self.state.watched.iter().enumerate() {
                            if i > 0 {
                                ui.separator();
                            }
                            let status = self.status_of(&w.addr);
                            ui.horizontal(|ui| {
                                let selected = self.selected.as_deref() == Some(&w.addr);
                                let name = truncate(&shown_name(w), 34);
                                let hint = match w.map.as_str() {
                                    "" => format!("{}\n{}", shown_name(w), w.addr),
                                    map => format!(
                                        "{}\n{} · {}",
                                        shown_name(w),
                                        map_title(map),
                                        w.addr
                                    ),
                                };
                                if ui
                                    .selectable_label(selected, name)
                                    .on_hover_text(hint)
                                    .clicked()
                                {
                                    pick = Some(w.addr.clone());
                                }
                                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                    if ui
                                        .small_button("×")
                                        .on_hover_text("Stop notifying")
                                        .clicked()
                                    {
                                        remove = Some(w.addr.clone());
                                    }
                                });
                            });
                            ui.horizontal(|ui| {
                                let (text, color) = describe(status.map(|(s, _)| s));
                                let text = RichText::new(text).small();
                                ui.label(match color {
                                    Some(color) => text.color(color),
                                    None => text.weak(),
                                });
                                // Not while it's still being found out.
                                if let Some((status, at)) = status
                                    && status != Status::Checking
                                {
                                    ui.label(
                                        RichText::new(format!("· checked {}", ago(at)))
                                            .small()
                                            .weak(),
                                    );
                                }
                                if status.is_some_and(|(s, _)| s.room()) {
                                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                        if ui.small_button("Join…").clicked() {
                                            join = Some(w.addr.clone());
                                        }
                                    });
                                }
                            });
                        }
                    });
                ui.add_space(4.0);
                ui.label(
                    RichText::new(format!(
                        "Checked about every {} seconds while the browser is open.",
                        watch::EVERY.as_secs()
                    ))
                    .small()
                    .weak(),
                );
            });
        if super::windows::escape_closes(ctx, id) {
            open = false;
        }
        if let Some(addr) = remove {
            self.toggle_watch(&addr);
        }
        if let Some(addr) = pick {
            self.ensure_listed(&addr);
            self.select(&addr, true);
        }
        if let Some(addr) = join {
            if self.ensure_listed(&addr) {
                self.select(&addr, true);
                self.open_join(&addr);
            } else {
                self.select(&addr, true);
            }
        }
        if !open {
            self.watch_list.window = false;
        }
        // Keep "checked … ago" ticking.
        ctx.request_repaint_after(Duration::from_secs(1));
    }

    /// The Notifications part of the settings window.
    pub(super) fn notifications_section(&mut self, ui: &mut egui::Ui) {
        ui.heading("Notifications");
        ui.label(
            RichText::new(
                "Right-click a server and pick \"Notify when there's room\" to put it on your \
                 watch list: you get a desktop notification when it has a free slot and no queue.",
            )
            .weak()
            .small(),
        );
        ui.horizontal(|ui| {
            if ui
                .checkbox(&mut self.settings.notify_sound, "Play a sound")
                .changed()
            {
                self.watch_list
                    .watcher
                    .set_sound(self.settings.notify_sound);
            }
            // One at a time: with no notification service, it takes a while to fail.
            let testing = self.watch_list.testing.load(Ordering::Relaxed);
            let label = if testing {
                "Sending…"
            } else {
                "Send a test notification"
            };
            if ui.add_enabled(!testing, egui::Button::new(label)).clicked() {
                let sound = self.settings.notify_sound;
                let testing = self.watch_list.testing.clone();
                testing.store(true, Ordering::Relaxed);
                self.spawn(move || {
                    let shown = notify::show(
                        "Test notification",
                        "Servers on your watch list are announced like this when they have room.",
                        sound,
                        None,
                    );
                    testing.store(false, Ordering::Relaxed);
                    match shown {
                        Ok(()) => Msg::Toast("Sent a test notification".into()),
                        Err(e) => {
                            log::warn!("couldn't show a notification: {e:#}");
                            Msg::Toast(format!(
                                "Couldn't show a notification ({e:#}); the browser will say it \
                                 here instead"
                            ))
                        }
                    }
                });
            }
        });
        if cfg!(windows) {
            ui.label(
                RichText::new("Windows shows them as coming from Windows PowerShell.")
                    .weak()
                    .small(),
            );
        }
    }
}

/// A watched server's name, or its address while that isn't known.
fn shown_name(w: &Watched) -> String {
    if w.name.is_empty() {
        w.addr.clone()
    } else {
        w.name.clone()
    }
}

/// How a watched server stands, in a few words, and its color (none: plain).
fn describe(status: Option<Status>) -> (String, Option<Color32>) {
    match status {
        None | Some(Status::Checking) => ("checking…".into(), None),
        Some(Status::Room { players, slots }) => {
            (format!("{players}/{slots}, has room"), Some(GOOD))
        }
        Some(Status::NoRoom {
            players,
            slots,
            queue: 0,
        }) => (format!("{players}/{slots}, full"), None),
        Some(Status::NoRoom {
            players,
            slots,
            queue,
        }) => (format!("{players}/{slots}, {queue} in queue"), None),
        Some(Status::Silent) => ("not answering".into(), Some(WARN)),
    }
}

fn ago(at: Instant) -> String {
    match at.elapsed().as_secs() {
        0..=4 => "just now".into(),
        secs @ 5..=59 => format!("{secs} s ago"),
        secs => format!("{} min ago", secs / 60),
    }
}

fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_string()
    } else {
        let cut: String = text.chars().take(max - 1).collect();
        format!("{cut}…")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn describes_how_a_server_stands() {
        assert_eq!(describe(None).0, "checking…");
        assert_eq!(
            describe(Some(Status::NoRoom {
                players: 60,
                slots: 60,
                queue: 3
            }))
            .0,
            "60/60, 3 in queue"
        );
        assert_eq!(
            describe(Some(Status::NoRoom {
                players: 60,
                slots: 60,
                queue: 0
            }))
            .0,
            "60/60, full"
        );
        assert_eq!(
            describe(Some(Status::Room {
                players: 57,
                slots: 60
            })),
            ("57/60, has room".into(), Some(GOOD))
        );
        assert_eq!(describe(Some(Status::Silent)).0, "not answering");
    }

    #[test]
    fn names_a_server_by_its_address_until_its_name_is_known() {
        let mut w = Watched {
            addr: "1.2.3.4:2303".into(),
            ..Default::default()
        };
        assert_eq!(shown_name(&w), "1.2.3.4:2303");
        w.name = "Some server".into();
        assert_eq!(shown_name(&w), "Some server");
        assert_eq!(truncate("abcdef", 4), "abc…");
        assert_eq!(truncate("abc", 4), "abc");
    }
}
