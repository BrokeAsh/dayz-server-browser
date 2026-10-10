//! The "Connect by address" window: finds a server from its IP or host name and port, including
//! servers the list doesn't have, and opens the join window for it. It can also look for servers
//! on the local network.

use eframe::egui::{self, RichText};
use std::net::{IpAddr, SocketAddr};

use super::filters::truncate;
use super::{App, ERROR, Msg, Then};
use crate::address;
use crate::lan::{self, Scan};
use crate::server::map_title;

#[derive(Default)]
pub struct ConnectWindow {
    text: String,
    /// The address being looked up, while one is.
    pub(super) asking: Option<String>,
    error: Option<String>,
    focused: bool,
    /// Another port to ask in a network search, as typed.
    scan_port: String,
    scanning: bool,
    scan: Option<Result<Scan, String>>,
}

impl ConnectWindow {
    pub(super) fn busy(&self) -> bool {
        self.asking.is_some()
    }

    pub fn failed(&mut self, error: String) {
        self.asking = None;
        self.error = Some(error);
    }

    pub fn scanned(&mut self, result: Result<Scan, String>) {
        if self.scanning {
            self.scanning = false;
            self.scan = Some(result);
        }
    }
}

impl App {
    pub(super) fn connect_window(&mut self, ctx: &egui::Context) {
        let Some(mut window) = self.connect_window.take() else {
            return;
        };
        let mut open = true;
        let mut go = false;
        let mut scan = None;
        let mut join_found = None;
        egui::Window::new("Connect by address")
            .collapsible(false)
            .resizable(false)
            .default_width(360.0)
            .open(&mut open)
            .show(ctx, |ui| {
                ui.label("Server address");
                ui.horizontal(|ui| {
                    let edit = ui.add_enabled(
                        !window.busy(),
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
                        !window.busy() && !window.text.trim().is_empty(),
                        egui::Button::new("Connect"),
                    );
                    go = (enter || button.clicked()) && !window.busy();
                    if window.busy() {
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
                    ui.label(RichText::new(e).color(ERROR));
                }
                ui.add_space(4.0);
                ui.separator();
                scan = network_section(ui, &mut window, &mut join_found);
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
            window.asking = Some(text.clone());
            self.spawn(move || Msg::Found {
                result: address::parse(&text)
                    .and_then(|(ip, port)| address::find(ip, port))
                    .map_err(|e| format!("{e:#}")),
                addr: text,
                then: Then::Join,
            });
        }
        if let Some(port) = scan {
            window.scanning = true;
            window.scan = None;
            self.spawn(move || Msg::LanScan(lan::scan(port).map_err(|e| format!("{e:#}"))));
        }
        if let Some(addr) = join_found.filter(|_| !window.busy()) {
            // Asked again, so the join window gets a fresh answer; it's remembered like a
            // server typed in.
            window.error = None;
            window.asking = Some(addr.clone());
            self.look_up(addr, Then::Join);
        }
        if open {
            self.connect_window = Some(window);
        }
    }

    /// The listed server at a typed IP and game or query port (port 2302 if left out).
    fn listed(&self, text: &str) -> Option<String> {
        let (ip, port) = match text.parse::<SocketAddr>() {
            Ok(addr) => (addr.ip(), addr.port()),
            Err(_) => (text.parse::<IpAddr>().ok()?, address::DEFAULT_PORT),
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

/// "Find servers on my network" and what it found. Returns the extra port to ask (if any) when
/// a search should start, and sets `join` to a found server's query address to join it.
fn network_section(
    ui: &mut egui::Ui,
    window: &mut ConnectWindow,
    join: &mut Option<String>,
) -> Option<Option<u16>> {
    let mut start = None;
    let typed = window.scan_port.trim();
    let port: Option<Option<u16>> = if typed.is_empty() {
        Some(None)
    } else {
        typed.parse::<u16>().ok().filter(|p| *p > 0).map(Some)
    };
    ui.horizontal(|ui| {
        let button = ui
            .add_enabled(
                !window.scanning && port.is_some(),
                egui::Button::new("Find servers on my network"),
            )
            .on_hover_text(format!(
                "Asks every address on your local network on ports {} and {}, and the port \
                 typed here, if any",
                lan::PORTS[0],
                lan::PORTS[1]
            ));
        if button.clicked() {
            start = port;
        }
        ui.label("Also port");
        ui.add_enabled(
            !window.scanning,
            egui::TextEdit::singleline(&mut window.scan_port)
                .hint_text("query")
                .desired_width(52.0),
        );
        if window.scanning {
            ui.spinner();
        }
    });
    if port.is_none() {
        ui.label(RichText::new("That port isn't a number from 1 to 65535").color(ERROR));
    }
    match &window.scan {
        None => {}
        Some(Err(e)) => {
            ui.label(RichText::new(e).color(ERROR));
        }
        Some(Ok(scan)) if scan.found.is_empty() => {
            let ports: Vec<String> = scan.ports.iter().map(u16::to_string).collect();
            ui.label(
                RichText::new(format!(
                    "No DayZ servers answered on {} (ports {}).",
                    scan.subnet,
                    ports.join(", ")
                ))
                .weak(),
            );
        }
        Some(Ok(scan)) => {
            egui::ScrollArea::vertical()
                .max_height(200.0)
                .id_salt("lan-servers")
                .show(ui, |ui| {
                    egui::Grid::new("lan-server-grid")
                        .num_columns(5)
                        .spacing([10.0, 4.0])
                        .striped(true)
                        .show(ui, |ui| {
                            for found in &scan.found {
                                let s = &found.server;
                                ui.label(truncate(&s.name, 34)).on_hover_text(&s.name);
                                ui.label(map_title(&s.map));
                                ui.label(format!("{}/{}", s.players, s.max_players));
                                ui.label(format!("{}:{}", s.ip, s.game_port));
                                if ui
                                    .add_enabled(!window.busy(), egui::Button::new("Join"))
                                    .clicked()
                                {
                                    *join = Some(s.addr());
                                }
                                ui.end_row();
                            }
                        });
                });
        }
    }
    start
}
