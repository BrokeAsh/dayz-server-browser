//! The server table: its columns and sortable header, the rows, the draggable column dividers,
//! the right-click menu on a row, pinging the servers on screen, and scrolling to the selected one.

use eframe::egui::{self, Align, Color32, Layout, RichText, Sense};
use egui_extras::{Column, TableBuilder};

use super::{App, ERROR, FAIR, GOOD, Live, PING_AGAIN, WARN};
use crate::geo;
use crate::server::{SortBy, map_title};
use crate::settings::COLUMNS;

impl App {
    pub(super) fn table(&mut self, ui: &mut egui::Ui) {
        // A re-sort or re-filter mustn't carry the selected server off screen: if it was on
        // screen, the list scrolls just enough to keep it there, and not at all while it's still
        // in view.
        let mut keep_in_view = false;
        if self.view_dirty {
            keep_in_view = self
                .selected
                .as_ref()
                .is_some_and(|selected| self.on_screen.contains(selected));
            self.rebuild_view();
        }
        let row_height = 22.0;
        let mut clicked: Option<(String, bool)> = None;
        let mut sort_clicked: Option<SortBy> = None;
        let mut favorite_clicked: Option<String> = None;
        let mut visible: Vec<String> = Vec::new();
        // Each row on screen, for its right-click menu.
        let mut menus: Vec<(String, egui::Response)> = Vec::new();
        let align = self.scroll_to_selected.take();
        let scroll_row = (align.is_some() || keep_in_view)
            .then(|| {
                let selected = self.selected.as_ref()?;
                self.view
                    .iter()
                    .position(|&i| self.servers[i].addr() == *selected)
            })
            .flatten();

        // The server name takes whatever room the other columns leave. In a narrow window the
        // optional columns make way, so the main ones always fit.
        let spacing = ui.spacing().item_spacing.x;
        let shown = shown_columns(&self.state.columns, ui.available_width(), spacing);
        let mut builder = TableBuilder::new(ui)
            .striped(true)
            .sense(Sense::click())
            .cell_layout(Layout::left_to_right(Align::Center))
            .column(Column::exact(STAR_WIDTH))
            .column(Column::remainder().at_least(NAME_MIN).clip(true));
        for &c in &shown {
            let width = column_width(&self.state.columns, c, &shown);
            builder = builder.column(Column::exact(width).clip(true));
        }
        builder = builder.column(Column::exact(LOCK_WIDTH));
        if let Some(row) = scroll_row {
            builder = builder.scroll_to_row(row, align);
        }
        let state = &self.state;
        let queue_column = shown.contains(&QUEUE);
        // Where each header cell is, for the column dividers drawn after the table.
        let mut cells: Vec<egui::Rect> = Vec::new();
        builder
            .header(24.0, |mut header| {
                let mut head = |header: &mut egui_extras::TableRow, label: &str, by: SortBy| {
                    header.col(|ui| {
                        cells.push(ui.max_rect());
                        let sorted = state.sort == by;
                        // The star's column has no room for an arrow: the star turns gold.
                        let star = by == SortBy::Favorite;
                        let arrow = match (sorted && !star, state.sort_descending) {
                            (false, _) => "",
                            (true, true) => " ⏷",
                            (true, false) => " ⏶",
                        };
                        let mut text = RichText::new(format!("{label}{arrow}")).strong();
                        if sorted && star {
                            text = text.color(Color32::GOLD);
                        }
                        if ui
                            .add(egui::Label::new(text).sense(Sense::click()))
                            .clicked()
                        {
                            sort_clicked = Some(by);
                        }
                    });
                };
                head(&mut header, "★", SortBy::Favorite);
                head(&mut header, "Server", SortBy::Name);
                for &c in &shown {
                    let (label, by) = HEADINGS[c];
                    head(&mut header, label, by);
                }
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
                        let marks = Marks {
                            note: &seen.note,
                            watched: state.is_watched(&addr),
                            suspect: self.suspect(&addr).map(|x| x.explain(&self.servers)),
                        };
                        marked_name(ui, &s.name, &marks);
                    });
                    let queue = seen.queue.or(s.queue).filter(|q| *q > 0);
                    for &c in &shown {
                        row.col(|ui| match c {
                            MAP => {
                                ui.label(map_title(&s.map));
                            }
                            PLAYERS => {
                                let (players, max) =
                                    seen.players.unwrap_or((s.players, s.max_players));
                                match queue.filter(|_| !queue_column) {
                                    // With no room for the Queue column, it's here.
                                    Some(q) => {
                                        ui.label(format!("{players}/{max} +{q}"))
                                            .on_hover_text(format!("{q} in queue"));
                                    }
                                    None => {
                                        ui.label(format!("{players}/{max}"));
                                    }
                                }
                            }
                            QUEUE => {
                                if let Some(queue) = queue {
                                    ui.label(queue.to_string());
                                }
                            }
                            TIME => {
                                ui.label(
                                    seen.time
                                        .as_ref()
                                        .or(s.time.as_ref())
                                        .map_or("", |t| t.as_str()),
                                );
                            }
                            PING => match live {
                                Some(Live { ping: Some(p), .. }) => {
                                    ui.label(RichText::new(format!("{p}")).color(ping_color(*p)));
                                }
                                Some(Live {
                                    at: Some(_),
                                    ping: None,
                                    ..
                                }) => {
                                    ui.label(RichText::new("—").weak())
                                        .on_hover_text("No answer");
                                }
                                _ => {
                                    ui.label(RichText::new("…").weak());
                                }
                            },
                            MODS => {
                                let text = if !s.mods.is_empty() {
                                    s.mods.len().to_string()
                                } else if s.modded {
                                    "yes".into()
                                } else {
                                    String::new()
                                };
                                ui.label(text);
                            }
                            _ => {
                                if let Some(code) = &s.country {
                                    ui.label(code)
                                        .on_hover_text(geo::name(code).unwrap_or(code));
                                }
                            }
                        });
                    }
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

        self.column_dividers(ui, &cells, &shown);

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

    /// The narrowest the list can be and still show its main columns (the star, the server name,
    /// the map, players, ping, and the padlock), with the central panel's margins.
    pub(super) fn table_min_width(&self, spacing: f32) -> f32 {
        let main: f32 = MAIN
            .iter()
            .map(|&c| column_width(&self.state.columns, c, &MAIN))
            .sum();
        STAR_WIDTH + NAME_MIN + main + LOCK_WIDTH + spacing * (MAIN.len() + 2) as f32 + 24.0
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
        let note = if self.state.notes.contains_key(addr) {
            "Edit note…"
        } else {
            "Add note…"
        };
        if ui.button(note).clicked() {
            self.edit_note(addr);
            ui.close();
        }
        let watch = if self.state.is_watched(addr) {
            "Stop notifying"
        } else {
            "Notify when there's room"
        };
        if ui.button(watch).clicked() {
            self.toggle_watch(addr);
            ui.close();
        }
    }

    /// Draggable dividers in the list's header. Dragging the one after a column resizes it, and
    /// the server name gives or takes the room; the one between the server name and the map moves
    /// the map's left edge. Double-clicking one puts that column back to its first width.
    fn column_dividers(&mut self, ui: &mut egui::Ui, cells: &[egui::Rect], shown: &[usize]) {
        // Header cells: star, server name, then the sized columns that show.
        let (Some(name), true) = (cells.get(1), cells.len() == 2 + shown.len()) else {
            return;
        };
        let slack = name.width() - NAME_MIN;
        let half_gap = ui.spacing().item_spacing.x / 2.0;
        for (i, cell) in cells.iter().enumerate().skip(1) {
            let x = cell.right() + half_gap;
            let rect = egui::Rect::from_x_y_ranges(x - 4.0..=x + 4.0, cell.y_range());
            // The divider after the server name belongs to the map (always shown, and first),
            // on its left.
            let (column, sign) = if i == 1 {
                (MAP, -1.0)
            } else {
                (shown[i - 2], 1.0)
            };
            // Named after its column, not its place: a column that comes back as another narrows
            // mustn't take over a drag.
            let response = ui
                .interact(
                    rect,
                    ui.id().with(("column divider", column, i == 1)),
                    Sense::click_and_drag(),
                )
                .on_hover_cursor(egui::CursorIcon::ResizeColumn);
            let stroke = if response.hovered() || response.dragged() {
                ui.visuals().widgets.hovered.bg_stroke
            } else {
                ui.visuals().widgets.noninteractive.bg_stroke
            };
            ui.painter().vline(x, cell.y_range().shrink(3.0), stroke);
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
const STAR_WIDTH: f32 = 22.0;
const LOCK_WIDTH: f32 = 20.0;

/// The sized columns, as indexes into `COLUMNS` and the saved widths.
const MAP: usize = 0;
const PLAYERS: usize = 1;
const QUEUE: usize = 2;
const TIME: usize = 3;
const PING: usize = 4;
const MODS: usize = 5;
const COUNTRY: usize = 6;
/// Their headings, and what clicking one sorts by.
const HEADINGS: [(&str, SortBy); COLUMNS.len()] = [
    ("Map", SortBy::Map),
    ("Players", SortBy::Players),
    ("Queue", SortBy::Queue),
    ("Time", SortBy::Time),
    ("Ping", SortBy::Ping),
    ("Mods", SortBy::Mods),
    ("Country", SortBy::Country),
];
/// The columns that always show.
const MAIN: [usize; 3] = [MAP, PLAYERS, PING];
/// The others, in the order they make way when the list is too narrow for all of them. A queue
/// then shows with the players.
const OPTIONAL: [usize; 4] = [TIME, COUNTRY, MODS, QUEUE];

/// The sized columns that fit in a list `room` wide, in order: all of them if they fit with the
/// star, the server name at its narrowest, and the padlock, otherwise without the optional ones
/// that don't.
fn shown_columns(widths: &[f32; COLUMNS.len()], room: f32, spacing: f32) -> Vec<usize> {
    let mut shown: Vec<usize> = (0..COLUMNS.len()).collect();
    let needs = |shown: &[usize]| {
        let columns: f32 = shown.iter().map(|&c| column_width(widths, c, shown)).sum();
        STAR_WIDTH + NAME_MIN + columns + LOCK_WIDTH + spacing * (shown.len() + 2) as f32
    };
    for drop in OPTIONAL {
        if needs(&shown) <= room {
            break;
        }
        shown.retain(|&c| c != drop);
    }
    shown
}

/// How much wider the Players column is when it shows the queue too.
const QUEUE_IN_PLAYERS: f32 = 22.0;

/// A column's width, from the saved ones: the Players column is wider when the Queue column
/// doesn't show, for the queue it shows instead.
fn column_width(widths: &[f32; COLUMNS.len()], column: usize, shown: &[usize]) -> f32 {
    if column == PLAYERS && !shown.contains(&QUEUE) {
        widths[column] + QUEUE_IN_PLAYERS
    } else {
        widths[column]
    }
}

/// What's marked after a server's name in the list.
struct Marks<'a> {
    /// The user's note on it (empty for none).
    note: &'a str,
    /// On the watch list.
    watched: bool,
    /// Why it looks fake or like a second listing, if it does.
    suspect: Option<String>,
}

/// A server's name in the list, followed by its marks: a note, the watch list's bell, and a
/// warning for a likely fake. A long name is cut short so the marks stay in view; hovering a
/// mark says what it means (the name with the note mark shows the note).
fn marked_name(ui: &mut egui::Ui, name: &str, marks: &Marks) {
    if marks.note.is_empty() && !marks.watched && marks.suspect.is_none() {
        ui.label(name);
        return;
    }
    let font = egui::TextStyle::Body.resolve(ui.style());
    let glyph = |ui: &egui::Ui, text: &str, color: Color32| {
        ui.painter()
            .layout_no_wrap(text.to_string(), font.clone(), color)
    };
    let bell = marks
        .watched
        .then(|| glyph(ui, "🔔", ui.visuals().text_color()));
    let warning = marks.suspect.as_ref().map(|_| glyph(ui, "⚠", WARN));
    let gap = ui.spacing().item_spacing.x;
    let mut marks_width = 0.0;
    if !marks.note.is_empty() {
        marks_width += super::notes::NOTE_MARK_WIDTH + gap;
    }
    for galley in [&bell, &warning].into_iter().flatten() {
        marks_width += galley.size().x + gap;
    }
    let room = (ui.available_width() - marks_width).max(0.0);
    let label = ui
        .allocate_ui(egui::vec2(room, ui.available_height()), |ui| {
            ui.add(
                egui::Label::new(name)
                    .truncate()
                    .show_tooltip_when_elided(marks.note.is_empty()),
            )
        })
        .inner;
    if !marks.note.is_empty() {
        let mark = super::notes::note_mark(ui);
        label.union(mark).on_hover_text(marks.note);
    }
    if let Some(bell) = bell {
        ui.add(egui::Label::new(bell))
            .on_hover_text("On your watch list: you'll be notified when it has room");
    }
    if let (Some(warning), Some(why)) = (warning, &marks.suspect) {
        ui.add(egui::Label::new(warning))
            .on_hover_text(format!("Likely fake\n{why}"));
    }
}

fn ping_color(ping: u32) -> Color32 {
    match ping {
        0..=60 => GOOD,
        61..=120 => FAIR,
        _ => ERROR,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A long name in a narrow column still leaves room for its marks.
    #[test]
    fn draws_marked_name() {
        let ctx = egui::Context::default();
        let marks = Marks {
            note: "A note",
            watched: true,
            suspect: Some("Listed twice.".into()),
        };
        for width in [NAME_MIN, 300.0] {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(width + 50.0, 100.0),
                )),
                ..Default::default()
            };
            let mut used = 0.0;
            let mut output = ctx.run_ui(input, |ui| {
                ui.allocate_ui(egui::vec2(width, 22.0), |ui| {
                    ui.horizontal(|ui| {
                        marked_name(ui, &"A very long server name ".repeat(5), &marks);
                        used = ui.min_rect().width();
                    });
                });
            });
            // Fonts would go to the GPU here.
            output.textures_delta.clear();
            assert!(used <= width + 1.0, "{used} > {width}");
        }
    }

    /// The optional columns make way, least useful first, and the main ones always stay.
    #[test]
    fn narrow_lists_drop_optional_columns() {
        let all: Vec<usize> = (0..COLUMNS.len()).collect();
        let needs = |shown: &[usize]| {
            let columns: f32 = shown
                .iter()
                .map(|&c| column_width(&COLUMNS, c, shown))
                .sum();
            STAR_WIDTH + NAME_MIN + columns + LOCK_WIDTH + 8.0 * (shown.len() + 2) as f32
        };
        assert_eq!(shown_columns(&COLUMNS, needs(&all), 8.0), all);
        assert_eq!(
            shown_columns(&COLUMNS, needs(&all) - 1.0, 8.0),
            [MAP, PLAYERS, QUEUE, PING, MODS, COUNTRY]
        );
        assert_eq!(
            shown_columns(&COLUMNS, needs(&[MAP, PLAYERS, QUEUE, PING, MODS]), 8.0),
            [MAP, PLAYERS, QUEUE, PING, MODS]
        );
        assert_eq!(shown_columns(&COLUMNS, 300.0, 8.0), MAIN);
        assert_eq!(shown_columns(&COLUMNS, 0.0, 8.0), MAIN);
    }
}
