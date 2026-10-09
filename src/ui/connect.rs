//! The "Connect by address" window: finds a server from its IP or host name and port, including
//! servers the list doesn't have, and opens the join window for it.

use eframe::egui::{self, Color32, RichText};
use std::net::{IpAddr, SocketAddr};

use super::{App, Msg};
use crate::address;

#[derive(Default)]
pub struct ConnectWindow {
    text: String,
    busy: bool,
    error: Option<String>,
    focused: bool,
}

impl ConnectWindow {
    pub fn failed(&mut self, error: String) {
        self.busy = false;
        self.error = Some(error);
    }
}

impl App {
    pub(super) fn connect_window(&mut self, ctx: &egui::Context) {
        let Some(mut window) = self.connect_window.take() else {
            return;
        };
        let mut open = true;
        let mut go = false;
        egui::Window::new("Connect by address")
            .collapsible(false)
            .resizable(false)
            .default_width(360.0)
            .open(&mut open)
            .show(ctx, |ui| {
                ui.label("Server address");
                ui.horizontal(|ui| {
                    let edit = ui.add_enabled(
                        !window.busy,
                        egui::TextEdit::singleline(&mut window.text)
                            .hint_text("203.0.113.5:2302")
                            .desired_width(240.0),
                    );
                    if !window.focused {
                        edit.request_focus();
                        window.focused = true;
                    }
                    let enter = edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    let button = ui.add_enabled(
                        !window.busy && !window.text.trim().is_empty(),
                        egui::Button::new("Connect"),
                    );
                    go = (enter || button.clicked()) && !window.busy;
                    if window.busy {
                        ui.spinner();
                    }
                });
                ui.label(
                    RichText::new(
                        "An IP or host name with the game port from the server's ad (2302 if \
                         left out). Its query port works too.",
                    )
                    .weak()
                    .small(),
                );
                if let Some(e) = &window.error {
                    ui.label(RichText::new(e).color(Color32::from_rgb(230, 120, 90)));
                }
            });
        if go && !window.text.trim().is_empty() {
            window.error = None;
            let text = window.text.trim().to_string();
            if let Some(addr) = self.listed(&text) {
                // Already in the list: no need to ask around.
                self.select(&addr, true);
                self.open_join(&addr);
                return;
            }
            window.busy = true;
            self.spawn(move || Msg::Found {
                result: address::parse(&text)
                    .and_then(|(ip, port)| address::find(ip, port))
                    .map_err(|e| format!("{e:#}")),
                join: true,
            });
        }
        if open {
            self.connect_window = Some(window);
        }
    }

    /// The listed server at a typed IP and game or query port (port 2302 if left out).
    fn listed(&self, text: &str) -> Option<String> {
        let (ip, port) = match text.parse::<SocketAddr>() {
            Ok(addr) => (addr.ip(), addr.port()),
            Err(_) => (text.parse::<IpAddr>().ok()?, 2302),
        };
        let ip = ip.to_string();
        let by = |game: bool| {
            self.servers.iter().find(|s| {
                s.ip == ip
                    && if game {
                        s.game_port == port
                    } else {
                        s.query_port == port
                    }
            })
        };
        by(true).or_else(|| by(false)).map(|s| s.addr())
    }
}
