//! The server table: its columns and sortable header, the rows, the draggable column dividers,
//! the right-click menu on a row, pinging the servers on screen, and scrolling to the selected one.

use eframe::egui::{self, Align, Color32, Layout, RichText, Sense};
use egui_extras::{Column, TableBuilder};

use super::{App, ERROR, FAIR, GOOD, Live, PING_AGAIN};
use crate::server::{SortBy, map_title};
use crate::settings::COLUMNS;

impl App {
    pub(super) fn table(&mut self, ui: &mut egui::Ui) {
        if self.view_dirty {
            // A re-sort or re-filter mustn't carry the selected server off screen: if it was on
            // screen, keep it there at about the same height.
            let keep = self.selected.as_ref().and_then(|selected| {
                let at = self.on_screen.iter().position(|a| a == selected)?;
                let height = at as f32 / self.on_screen.len().max(1) as f32;
                Some(if height < 0.33 {
                    Align::Min
                } else if height > 0.66 {
                    Align::Max
                } else {
                    Align::Center
                })
            });
            self.rebuild_view();
            if keep.is_some() {
                self.scroll_to_selected = keep;
            }
        }
        let row_height = 22.0;
        let mut clicked: Option<(String, bool)> = None;
        let mut sort_clicked: Option<SortBy> = None;
        let mut favorite_clicked: Option<String> = None;
        let mut visible: Vec<String> = Vec::new();
        // Each row on screen, for its right-click menu.
        let mut menus: Vec<(String, egui::Response)> = Vec::new();
        let scroll_row = self.scroll_to_selected.take().and_then(|align| {
            let selected = self.selected.as_ref()?;
            let row = self
                .view
                .iter()
                .position(|&i| self.servers[i].addr() == *selected)?;
            Some((row, align))
        });

        // The server name takes whatever room the other columns leave, so the table fits unless
        // the window is narrower than the columns' minimums.
        let columns = self.state.columns;
        let mut builder = TableBuilder::new(ui)
            .striped(true)
            .sense(Sense::click())
            .cell_layout(Layout::left_to_right(Align::Center))
            .column(Column::exact(22.0))
            .column(Column::remainder().at_least(NAME_MIN).clip(true));
        for width in columns {
            builder = builder.column(Column::exact(width).clip(true));
        }
        builder = builder.column(Column::exact(LOCK_WIDTH));
        if let Some((row, align)) = scroll_row {
            builder = builder.scroll_to_row(row, Some(align));
        }
        let state = &self.state;
        // Where each header cell is, for the column dividers drawn after the table.
        let mut cells: Vec<egui::Rect> = Vec::new();
        builder
            .header(24.0, |mut header| {
                let mut head = |header: &mut egui_extras::TableRow, label: &str, by: SortBy| {
                    header.col(|ui| {
                        cells.push(ui.max_rect());
                        let arrow = if state.sort == by {
                            if state.sort_descending {
                                " ⏷"
                            } else {
                                " ⏶"
                            }
                        } else {
                            ""
                        };
                        if ui
                            .add(
                                egui::Label::new(RichText::new(format!("{label}{arrow}")).strong())
                                    .sense(Sense::click()),
                            )
                            .clicked()
                        {
                            sort_clicked = Some(by);
                        }
                    });
                };
                head(&mut header, "★", SortBy::Favorite);
                head(&mut header, "Server", SortBy::Name);
                head(&mut header, "Map", SortBy::Map);
                head(&mut header, "Players", SortBy::Players);
                head(&mut header, "Queue", SortBy::Queue);
                head(&mut header, "Time", SortBy::Time);
                head(&mut header, "Ping", SortBy::Ping);
                head(&mut header, "Mods", SortBy::Mods);
                header.col(|ui| {
                    let color = ui.visuals().strong_text_color();
                    padlock(ui, color).on_hover_text("Needs a password");
                });
            })
            .body(|body| {
                body.rows(row_height, self.view.len(), |mut row| {
                    let s = &self.servers[self.view[row.index()]];
                    let addr = s.addr();
                    let live = self.live.get(&addr);
                    let seen = self.seen(s);
                    visible.push(addr.clone());
                    row.set_selected(self.selected.as_deref() == Some(&addr));
                    let favorite = seen.favorite;
                    row.col(|ui| {
                        let star = if favorite { "★" } else { "☆" };
                        let color = if favorite {
                            Color32::GOLD
                        } else {
                            ui.visuals().weak_text_color()
                        };
                        if ui
                            .add(
                                egui::Label::new(RichText::new(star).color(color))
                                    .sense(Sense::click()),
                            )
                            .clicked()
                        {
                            favorite_clicked = Some(addr.clone());
                        }
                    });
                    row.col(|ui| {
                        if state.is_watched(&addr) {
                            ui.label(RichText::new("🔔").small().weak())
                                .on_hover_text("You'll be notified when it has room");
                        }
                        ui.label(&s.name);
                    });
                    row.col(|ui| {
                        ui.label(map_title(&s.map));
                    });
                    row.col(|ui| {
                        let (players, max) = seen.players.unwrap_or((s.players, s.max_players));
                        ui.label(format!("{players}/{max}"));
                    });
                    row.col(|ui| {
                        if let Some(queue) = seen.queue.or(s.queue).filter(|q| *q > 0) {
                            ui.label(queue.to_string());
                        }
                    });
                    row.col(|ui| {
                        ui.label(
                            seen.time
                                .as_ref()
                                .or(s.time.as_ref())
                                .map_or("", |t| t.as_str()),
                        );
                    });
                    row.col(|ui| match live {
                        Some(Live { ping: Some(p), .. }) => {
                            ui.label(RichText::new(format!("{p}")).color(ping_color(*p)));
                        }
                        Some(Live {
                            at: Some(_),
                            ping: None,
                            ..
                        }) => {
                            ui.label(RichText::new("—").weak())
                                .on_hover_text("no answer");
                        }
                        _ => {
                            ui.label(RichText::new("…").weak());
                        }
                    });
                    row.col(|ui| {
                        let text = if !s.mods.is_empty() {
                            s.mods.len().to_string()
                        } else if s.modded {
                            "yes".into()
                        } else {
                            String::new()
                        };
                        ui.label(text);
                    });
                    row.col(|ui| {
                        if s.password || seen.locked {
                            let color = ui.visuals().text_color();
                            padlock(ui, color).on_hover_text("Needs a password");
                        }
                    });
                    let response = row.response();
                    menus.push((addr.clone(), response.clone()));
                    if response.double_clicked() {
                        clicked = Some((addr, true));
                    } else if response.clicked() {
                        clicked = Some((addr, false));
                    }
                });
            });

        self.column_dividers(ui, &cells);

        // Ping what's on screen (not too often).
        for addr in visible.iter().cloned() {
            let stale = self
                .live
                .get(&addr)
                .and_then(|l| l.at)
                .is_none_or(|t| t.elapsed() > PING_AGAIN);
            let asked_first = self.pinging.get(&addr).copied().unwrap_or(false);
            if stale && !asked_first {
                self.pinging.insert(addr.clone(), true);
                self.pinger.request(addr);
            }
        }
        self.on_screen = visible;
        if let Some(by) = sort_clicked {
            if self.state.sort == by {
                self.state.sort_descending = !self.state.sort_descending;
            } else {
                self.state.sort = by;
                // Biggest first for counts (players, queue, mods); otherwise smallest first:
                // favorites first, fastest, earliest, A to Z.
                self.state.sort_descending =
                    matches!(by, SortBy::Players | SortBy::Queue | SortBy::Mods);
            }
            self.filters_changed();
        }
        if let Some(addr) = favorite_clicked {
            self.toggle_favorite(&addr);
        }
        if let Some((addr, join)) = clicked {
            self.select(&addr, false);
            if join {
                self.open_join(&addr);
            }
        }
        // The right-click menus, once the table no longer borrows the app. Each belongs to its
        // server rather than its row, so a re-sort while one is open can't aim it at another.
        for (addr, response) in menus {
            egui::Popup::context_menu(&response)
                .id(egui::Id::new(("row menu", &addr)))
                .show(|ui| self.row_menu(ui, &addr));
        }
    }

    /// The right-click menu on a server's row. Each item does its thing and closes the menu.
    fn row_menu(&mut self, ui: &mut egui::Ui, addr: &str) {
        if ui.button("Join…").clicked() {
            self.open_join(addr);
            ui.close();
        }
        let favorite = if self.state.is_favorite(addr) {
            "Remove from favorites"
        } else {
            "Add to favorites"
        };
        if ui.button(favorite).clicked() {
            self.toggle_favorite(addr);
            ui.close();
        }
        if ui.button("Copy address").clicked() {
            // The address players type: the game port, not the query port.
            if let Some(&i) = self.index.get(addr) {
                let s = &self.servers[i];
                ui.ctx().copy_text(format!("{}:{}", s.ip, s.game_port));
                self.toast("Address copied");
            }
            ui.close();
        }
        let watch = if self.state.is_watched(addr) {
            "Stop notifying"
        } else {
            "Notify me when there's room"
        };
        if ui.button(watch).clicked() {
            self.toggle_watch(addr);
            ui.close();
        }
    }

    /// Draggable dividers in the list's header. Dragging the one after a column resizes it, and
    /// the server name gives or takes the room; the one between the server name and the map moves
    /// the map's left edge. Double-clicking one puts that column back to its first width.
    fn column_dividers(&mut self, ui: &mut egui::Ui, cells: &[egui::Rect]) {
        // Header cells: star, server name, then the sized columns.
        let (Some(name), true) = (cells.get(1), cells.len() == 2 + COLUMNS.len()) else {
            return;
        };
        let slack = name.width() - NAME_MIN;
        let half_gap = ui.spacing().item_spacing.x / 2.0;
        for (i, cell) in cells.iter().enumerate().skip(1) {
            let x = cell.right() + half_gap;
            let rect = egui::Rect::from_x_y_ranges(x - 4.0..=x + 4.0, cell.y_range());
            let response = ui
                .interact(
                    rect,
                    ui.id().with(("column divider", i)),
                    Sense::click_and_drag(),
                )
                .on_hover_cursor(egui::CursorIcon::ResizeColumn);
            let stroke = if response.hovered() || response.dragged() {
                ui.visuals().widgets.hovered.bg_stroke
            } else {
                ui.visuals().widgets.noninteractive.bg_stroke
            };
            ui.painter().vline(x, cell.y_range().shrink(3.0), stroke);
            // The divider after the server name belongs to the map, on its left.
            let (column, sign) = if i == 1 { (0, -1.0) } else { (i - 2, 1.0) };
            let width = &mut self.state.columns[column];
            if response.double_clicked() {
                *width = COLUMNS[column];
                self.state_changed();
            } else if response.dragged() {
                let grow = (response.drag_delta().x * sign).min(slack.max(0.0));
                *width = (*width + grow).max(COLUMN_MIN);
                self.state_changed();
            }
        }
    }
}

/// A small padlock, drawn rather than taken from the emoji font, whose lock is hard to make out.
fn padlock(ui: &mut egui::Ui, color: Color32) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(11.0, 14.0), Sense::hover());
    let painter = ui.painter();
    let body = egui::Rect::from_min_max(
        egui::pos2(rect.left(), rect.top() + 6.0),
        rect.right_bottom(),
    );
    painter.rect_filled(body, 2.0, color);
    // The shackle: an arch whose legs reach into the body.
    let (center, radius) = (egui::pos2(rect.center().x, rect.top() + 4.5), 3.2);
    let mut shackle = vec![egui::pos2(center.x - radius, body.top() + 1.0)];
    shackle.extend((0..=12).map(|i| {
        let angle = std::f32::consts::PI * i as f32 / 12.0;
        egui::pos2(
            center.x - radius * angle.cos(),
            center.y - radius * angle.sin(),
        )
    }));
    shackle.push(egui::pos2(center.x + radius, body.top() + 1.0));
    painter.add(egui::Shape::line(shackle, egui::Stroke::new(1.6, color)));
    let keyhole = ui.visuals().extreme_bg_color;
    painter.circle_filled(
        egui::pos2(body.center().x, body.center().y - 0.6),
        1.3,
        keyhole,
    );
    painter.line_segment(
        [
            egui::pos2(body.center().x, body.center().y),
            egui::pos2(body.center().x, body.center().y + 2.0),
        ],
        egui::Stroke::new(1.2, keyhole),
    );
    response
}

/// Narrowest the server name and the other list columns get.
const NAME_MIN: f32 = 120.0;
const COLUMN_MIN: f32 = 30.0;
const LOCK_WIDTH: f32 = 20.0;

fn ping_color(ping: u32) -> Color32 {
    match ping {
        0..=60 => GOOD,
        61..=120 => FAIR,
        _ => ERROR,
    }
}
