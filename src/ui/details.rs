//! The details panel on the right: the selected server's address, Join and Favorite, its info,
//! and its mods; or, for a server the list doesn't have, a way to ask it directly.

use eframe::egui::{self, RichText, Sense};

use super::{App, ERROR, Fetch, Then, WARN, open_workshop, state_badge};
use crate::a2s;
use crate::server::map_title;

impl App {
    /// The details panel for a server (picked from Recent) that the list doesn't have.
    fn missing_server(&mut self, ui: &mut egui::Ui, addr: &str) {
        ui.add_space(4.0);
        ui.label("This server isn't in the current list.");
        if self.looking_up.as_deref() == Some(addr) {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Asking it directly…");
            });
            return;
        }
        if let Some((failed, e)) = &self.lookup_failed
            && failed == addr
        {
            ui.label(RichText::new(format!("It didn't answer: {e}")).color(ERROR));
        }
        if ui.button("Ask the server").clicked() {
            self.lookup_failed = None;
            self.looking_up = Some(addr.to_string());
            self.look_up(addr.to_string(), Then::Show);
        }
    }

    pub(super) fn details_panel(&mut self, ui: &mut egui::Ui) {
        let Some(addr) = self.selected.clone() else {
            ui.add_space(20.0);
            ui.vertical_centered(|ui| {
                ui.label(RichText::new("Select a server to see its details and mods.").weak());
            });
            return;
        };
        let Some(&i) = self.index.get(&addr) else {
            self.missing_server(ui, &addr);
            return;
        };
        let s = self.servers[i].clone();
        let live = self.live.get(&addr);
        let info = live.and_then(|l| l.info.clone());
        let ping = live.and_then(|l| l.ping);

        ui.add_space(4.0);
        ui.label(RichText::new(&s.name).heading());
        ui.horizontal(|ui| {
            ui.label(RichText::new(format!("{}:{}", s.ip, s.game_port)).monospace());
            if ui
                .small_button("Copy")
                .on_hover_text("Copy the address")
                .clicked()
            {
                ui.ctx().copy_text(format!("{}:{}", s.ip, s.game_port));
                self.toast("Address copied");
            }
        });
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            let join = ui.add(egui::Button::new(RichText::new("  Join  ").strong()));
            if join.clicked() {
                self.open_join(&addr);
            }
            let favorite = self.state.is_favorite(&addr);
            let label = if favorite {
                "★ Favorite"
            } else {
                "☆ Favorite"
            };
            if ui.selectable_label(favorite, label).clicked() {
                self.toggle_favorite(&addr);
            }
        });
        if self.state.added.contains(&addr) && self.only_added(&addr) {
            ui.horizontal(|ui| {
                ui.label(RichText::new("Added by address").weak().small());
                if ui
                    .small_button("Remove")
                    .on_hover_text("Take it off the list")
                    .clicked()
                {
                    self.forget_added(&addr);
                }
            });
        }
        ui.add_space(8.0);

        let tags = info.as_ref().map(|i| a2s::parse_tags(&i.keywords));
        let same_version = self.seen(&s).same_version;
        egui::Grid::new("details")
            .num_columns(2)
            .spacing([12.0, 4.0])
            .show(ui, |ui| {
                let row = |ui: &mut egui::Ui, k: &str, v: String| {
                    ui.label(RichText::new(k).weak());
                    ui.label(v);
                    ui.end_row();
                };
                row(ui, "Map", map_title(&s.map));
                let (players, max) = info
                    .as_ref()
                    .map(|i| (i.players, i.max_players))
                    .unwrap_or((s.players, s.max_players));
                let queue = tags
                    .as_ref()
                    .and_then(|t| t.queue)
                    .or(s.queue)
                    .filter(|q| *q > 0);
                row(
                    ui,
                    "Players",
                    match queue {
                        Some(q) => format!("{players} / {max}, {q} waiting"),
                        None => format!("{players} / {max}"),
                    },
                );
                if let Some(time) = tags
                    .as_ref()
                    .and_then(|t| t.time.clone())
                    .or(s.time.clone())
                {
                    row(ui, "Time", time);
                }
                row(
                    ui,
                    "Ping",
                    match ping {
                        Some(p) => format!("{p} ms"),
                        None => "—".into(),
                    },
                );
                let first = tags
                    .as_ref()
                    .map(|t| t.first_person)
                    .unwrap_or(s.first_person);
                row(
                    ui,
                    "Camera",
                    if first {
                        "First person only"
                    } else {
                        "First and third person"
                    }
                    .into(),
                );
                let version = info
                    .as_ref()
                    .map(|i| i.version.clone())
                    .filter(|v| !v.is_empty())
                    .unwrap_or(s.version.clone());
                if !version.is_empty() {
                    ui.label(RichText::new("Version").weak());
                    match (&self.game_version, same_version) {
                        (Some(game), Some(false)) => {
                            ui.label(RichText::new(&version).color(WARN))
                                .on_hover_text(format!(
                                    "You have {game}. DayZ won't join a server on another version."
                                ));
                        }
                        _ => {
                            ui.label(version);
                        }
                    }
                    ui.end_row();
                }
                let battleye = tags.as_ref().map(|t| t.battleye).unwrap_or(s.battleye);
                row(ui, "BattlEye", if battleye { "Yes" } else { "No" }.into());
                let locked = s.password || info.as_ref().is_some_and(|i| i.password);
                row(ui, "Password", if locked { "Yes" } else { "No" }.into());
            });

        ui.add_space(10.0);
        let (mods, origin) = self.mods_for(&s);
        let loading = matches!(self.rules.get(&addr), Some(Fetch::Loading));
        ui.horizontal(|ui| {
            let count = mods.as_ref().map_or(0, Vec::len);
            ui.label(RichText::new(format!("Mods ({count})")).strong());
            if loading {
                ui.spinner();
            } else if !origin.is_empty() {
                ui.label(RichText::new(origin).weak().small());
            }
        });
        if let Some(Fetch::Failed(e)) = self.rules.get(&addr) {
            ui.label(
                RichText::new(format!("The server didn't send its mod list: {e}"))
                    .weak()
                    .small(),
            );
        }
        if let Some(Fetch::Done(rules)) = self.rules.get(&addr)
            && rules.truncated
        {
            ui.label(
                RichText::new("The server's list was cut short; more mods may be needed.")
                    .weak()
                    .small(),
            );
        }
        match mods {
            Some(mods) if mods.is_empty() => {
                ui.label(RichText::new("None (vanilla)").weak());
            }
            Some(mods) => {
                egui::ScrollArea::vertical()
                    .id_salt("mods")
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        for m in &mods {
                            ui.horizontal(|ui| {
                                let (icon, color, tip) = state_badge(self.mod_state(m));
                                ui.label(RichText::new(icon).color(color))
                                    .on_hover_text(tip);
                                // Long names are cut to the panel's width rather than widening it;
                                // dragging the panel wider shows more, and hovering shows all of it.
                                let color = if m.id != 0 {
                                    ui.visuals().hyperlink_color
                                } else {
                                    ui.visuals().text_color()
                                };
                                let name = ui.add(
                                    egui::Label::new(RichText::new(&m.name).color(color))
                                        .truncate()
                                        .sense(Sense::click()),
                                );
                                if m.id != 0 {
                                    if name.clicked() {
                                        open_workshop(m.id);
                                    }
                                    name.on_hover_cursor(egui::CursorIcon::PointingHand)
                                        .on_hover_text(format!(
                                            "{}\nClick to open in the Steam Workshop",
                                            m.name
                                        ));
                                } else {
                                    name.on_hover_text(&m.name);
                                }
                            });
                        }
                    });
            }
            None if !loading => {
                ui.label(RichText::new("Unknown until the server answers.").weak());
            }
            None => {}
        }
    }
}
