//! The details panel on the right: the selected server's address, Join and Favorite, its info,
//! and its mods; or, for a server the list doesn't have, a way to ask it directly.

use eframe::egui::{self, RichText, Sense};

use super::{App, ERROR, Fetch, Then, WARN, open_workshop, sentence, state_badge};
use crate::a2s::{self, Info};
use crate::server::{Server, map_title};

/// The details panel's width at first (leaving the list room for all its columns at 1200 px)...
const DETAILS_WIDTH: f32 = 330.0;
/// ...the narrowest it can be dragged...
const DETAILS_MIN: f32 = 260.0;
/// ...and the narrowest it gets in a small window, leaving the list room.
const DETAILS_SMALLEST: f32 = 200.0;
/// About how wide the names in the details grid ("Time speed") are, with the gap after them.
const DETAILS_KEYS: f32 = 90.0;

impl App {
    /// The details panel on the right. It keeps the width it's dragged to (rather than taking
    /// the width of what it shows), except that in a narrow window it gets narrower, so the list
    /// keeps room for its main columns.
    pub(super) fn details_side_panel(&mut self, ui: &mut egui::Ui) {
        let id = egui::Id::new("details");
        let wanted_id = id.with("wanted width");
        let wanted = ui
            .data(|d| d.get_temp::<f32>(wanted_id))
            .unwrap_or(DETAILS_WIDTH);
        let spacing = ui.spacing().item_spacing.x;
        let room = (ui.available_width() - self.table_min_width(spacing)).max(DETAILS_SMALLEST);
        // egui would keep the width the content took last; this one is the width picked.
        let width = wanted.min(room);
        ui.data_mut(|d| {
            d.insert_persisted(
                id,
                egui::containers::panel::PanelState {
                    outer_rect: egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(width, 1.0)),
                },
            )
        });
        let panel = egui::Panel::right(id)
            .resizable(true)
            .size_range(DETAILS_MIN.min(room)..=room)
            .show(ui, |ui| {
                // Something too wide is cut off rather than widening the panel, which would
                // also put the list over its left side.
                let rect = ui.available_rect_before_wrap();
                let mut inside = ui.new_child(egui::UiBuilder::new().max_rect(rect));
                inside.set_clip_rect(rect.intersect(ui.clip_rect()));
                self.details_panel(&mut inside);
                ui.advance_cursor_after_rect(rect);
            });
        // The panel's drag handle (egui's id for it).
        let resize = ui.ctx().read_response(id.with("__resize"));
        if resize.is_some_and(|r| r.dragged() || r.drag_stopped()) {
            let picked = panel.response.rect.width();
            ui.data_mut(|d| d.insert_temp(wanted_id, picked));
        }
    }

    /// The details panel for a server the list doesn't have (picked from Recent or the watch
    /// list): its name and address, a way to ask it directly, and one to forget a recent one.
    fn missing_server(&mut self, ui: &mut egui::Ui, addr: &str) {
        let recent = self.state.recent.iter().find(|r| r.addr == addr);
        let watched = self.state.watched.iter().find(|w| w.addr == addr);
        let name = recent
            .map(|r| r.name.as_str())
            .or(watched.map(|w| w.name.as_str()))
            .filter(|n| !n.is_empty());
        let in_recent = recent.is_some();
        ui.add_space(4.0);
        if let Some(name) = name {
            ui.label(RichText::new(name).heading());
        }
        ui.label(RichText::new(addr).monospace())
            .on_hover_text("Its query address");
        ui.add_space(6.0);
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
            ui.label(RichText::new(sentence(e)).color(ERROR));
        }
        ui.horizontal_wrapped(|ui| {
            if ui.button("Ask the server").clicked() {
                self.lookup_failed = None;
                self.looking_up = Some(addr.to_string());
                self.look_up(addr.to_string(), Then::Show);
            }
            if in_recent
                && ui
                    .button("Remove from Recent")
                    .on_hover_text("Take it off the Recent list")
                    .clicked()
            {
                self.state.recent.retain(|r| r.addr != addr);
                self.state_changed();
                self.selected = None;
            }
        });
    }

    pub(super) fn details_panel(&mut self, ui: &mut egui::Ui) {
        self.note_follows_selection();
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
        self.suspect_note(ui, &addr);
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
        // Labels that change when clicked come last, so nothing moves under the pointer; in a
        // narrow panel the buttons go on two lines.
        ui.horizontal_wrapped(|ui| {
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
            self.watch_toggle(ui, &addr);
            self.note_button(ui, &addr);
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
        // The rest scrolls, so a short window still reaches the mods; each server's from the top.
        egui::ScrollArea::vertical()
            .id_salt(("details", &addr))
            .auto_shrink([false, false])
            .show(ui, |ui| self.details_body(ui, &addr, &s, info, ping));
    }

    /// The scrolling part of the details panel: the server's info, the note, the player history,
    /// and the mods.
    fn details_body(
        &mut self,
        ui: &mut egui::Ui,
        addr: &str,
        s: &Server,
        info: Option<Info>,
        ping: Option<u32>,
    ) {
        let tags = info.as_ref().map(|i| a2s::parse_tags(&i.keywords));
        let same_version = self.seen(s).same_version;
        // Long values wrap, rather than widening the panel past the window. (The names on the
        // left are short.)
        let values = (ui.available_width() - DETAILS_KEYS).max(60.0);
        egui::Grid::new("details")
            .num_columns(2)
            .spacing([12.0, 4.0])
            .max_col_width(values)
            .show(ui, |ui| {
                let row = |ui: &mut egui::Ui, k: &str, v: String| {
                    ui.label(RichText::new(k).weak());
                    ui.add(egui::Label::new(v).wrap());
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
                        Some(q) => format!("{players} / {max}, {q} in queue"),
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
                let (first, reported) = match (&info, &tags) {
                    (Some(i), Some(t)) => (
                        self.flipped.first_person(
                            &i.name,
                            if i.version.is_empty() {
                                &s.version
                            } else {
                                &i.version
                            },
                            t.first_person,
                        ),
                        t.first_person,
                    ),
                    _ => (s.first_person, s.reported_first_person),
                };
                ui.label(RichText::new("Camera").weak());
                let camera = ui.add(
                    egui::Label::new(if first {
                        "First person only"
                    } else {
                        "First and third person"
                    })
                    .wrap(),
                );
                if first != reported {
                    camera.on_hover_text(
                        "The server reports the opposite, but servers on this version of DayZ \
                         get it backwards, so this goes by its name or turns the report around.",
                    );
                }
                ui.end_row();
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
                            ui.add(egui::Label::new(version).wrap());
                        }
                    }
                    ui.end_row();
                }
                let battleye = tags.as_ref().map(|t| t.battleye).unwrap_or(s.battleye);
                row(ui, "BattlEye", if battleye { "Yes" } else { "No" }.into());
                let locked = s.password || info.as_ref().is_some_and(|i| i.password);
                row(ui, "Password", if locked { "Yes" } else { "No" }.into());
                self.more_details(ui, s, tags.as_ref());
            });
        self.note_section(ui, addr);
        self.history_section(ui, addr);

        ui.add_space(10.0);
        let (mods, origin) = self.mods_for(s);
        let loading = matches!(self.rules.get(addr), Some(Fetch::Loading));
        ui.horizontal(|ui| {
            let count = mods.as_ref().map_or(0, Vec::len);
            ui.label(RichText::new(format!("Mods ({count})")).strong());
            if loading {
                ui.spinner();
            } else if !origin.is_empty() {
                ui.label(RichText::new(origin).weak().small());
            }
        });
        if let Some(Fetch::Failed(e)) = self.rules.get(addr) {
            ui.label(
                RichText::new(format!("The server didn't send its mod list: {e}"))
                    .weak()
                    .small(),
            );
        }
        if let Some(Fetch::Done(rules)) = self.rules.get(addr)
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
            }
            None if !loading => {
                ui.label(RichText::new("Unknown until the server answers.").weak());
            }
            None => {}
        }
    }
}
