//! The bars around the list: the top bar (Refresh, Connect by address, Play offline, the search,
//! Settings, News, and the Update button) and the status bar (list counts and age, likely fakes
//! hidden, pinging, the map overlay, DayZ, the watch list).

use eframe::egui::{self, Align, Layout, RichText};
use std::time::SystemTime;

use super::{App, ERROR, GOOD, OverlayStatus, WARN, connect, sentence, settings};
use crate::settings::Source;

/// The search box's width when there's room for it...
const SEARCH_WIDTH: f32 = 260.0;
/// ...and the narrowest it gets in a small window.
const SEARCH_MIN: f32 = 80.0;
/// The widest the status bar's word on DayZ gets (a long error is cut short; hovering shows it).
const DAYZ_STATUS_MAX: f32 = 240.0;
/// Below this width the status bar leaves out "DayZ found".
const NARROW: f32 = 1000.0;

impl App {
    pub(super) fn top_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let refresh = ui
                .add_enabled(self.refresh_allowed(), egui::Button::new("⟳ Refresh"))
                .on_disabled_hover_text(if self.list.loading {
                    "Getting the list…"
                } else {
                    "Refreshed less than 30 seconds ago"
                });
            if refresh.clicked() {
                self.load_list(true);
            }
            if self.list.loading {
                ui.spinner();
            }
            if ui
                .button("Connect by address")
                .on_hover_text("Join a server by its IP and port, even if it isn't listed")
                .clicked()
                && self.connect_window.is_none()
            {
                self.connect_window = Some(connect::ConnectWindow::default());
            }
            if ui
                .button("Play offline")
                .on_hover_text("Play DayZ on your own, with no server (Community Offline Mode)")
                .clicked()
            {
                self.open_offline();
            }
            if ui
                .button("Mods")
                .on_hover_text(
                    "Your Workshop mods: their sizes, which servers use them, and cleaning up",
                )
                .clicked()
            {
                self.open_mods_page();
            }
            ui.separator();
            ui.label("Search");
            // The search gets what the buttons on the right leave it (going by how wide they
            // were last frame), so it never runs under them.
            let right_id = ui.id().with("top bar right");
            let right = ui.data(|d| d.get_temp::<f32>(right_id)).unwrap_or(0.0);
            let gap = ui.spacing().item_spacing.x;
            let room = ui.available_width() - right - 2.0 * gap;
            let search = ui.add(
                egui::TextEdit::singleline(&mut self.state.filters.search)
                    .hint_text("name, map, mod, or address")
                    .desired_width(room.clamp(SEARCH_MIN, SEARCH_WIDTH)),
            );
            if search.changed() {
                self.filters_changed();
            }
            let buttons = ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.button("⚙ Settings").clicked() && self.settings_window.is_none() {
                    self.settings_window = Some(settings::SettingsWindow::new(&self.settings));
                }
                self.news_button(ui);
                self.update_button(ui);
                ui.min_rect().width()
            });
            let used = buttons.inner;
            if (used - right).abs() > 0.5 {
                ui.data_mut(|d| d.insert_temp(right_id, used));
                ui.ctx().request_repaint();
            }
        });
    }

    pub(super) fn status_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            // The right-hand part first, so the left-hand one knows how much room it has.
            let narrow = ui.available_width() < NARROW;
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let (text, color) = match &self.overlay {
                    OverlayStatus::Off => ("Map overlay off".to_string(), None),
                    OverlayStatus::Checking => ("Map overlay: starting…".into(), None),
                    OverlayStatus::NotFound => ("Map overlay missing".into(), Some(WARN)),
                    OverlayStatus::Running => ("Map overlay ready".into(), Some(GOOD)),
                    OverlayStatus::Failed(e) => (format!("Map overlay: {e}"), Some(ERROR)),
                };
                let label = match color {
                    Some(c) => RichText::new(text).color(c),
                    None => RichText::new(text).weak(),
                };
                ui.label(label);
                if !(narrow && self.install.is_ok()) {
                    ui.separator();
                }
                match &self.install {
                    // Only where there's room: it's the usual case.
                    Ok(_) if !narrow => {
                        ui.label(RichText::new("DayZ found").weak());
                    }
                    Ok(_) => {}
                    Err(e) => {
                        let e = sentence(e);
                        ui.scope(|ui| {
                            ui.set_max_width(DAYZ_STATUS_MAX);
                            ui.add(egui::Label::new(RichText::new(&e).color(ERROR)).truncate())
                                .on_hover_text(&e);
                        });
                    }
                }
                self.watch_button(ui);
                ui.separator();
                ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                    // What doesn't fit is cut off rather than drawn under the right-hand part.
                    ui.set_clip_rect(ui.max_rect().intersect(ui.clip_rect()));
                    self.status_left(ui);
                });
            });
        });
    }

    /// The status bar's left-hand part: the counts, and what's going on with the list.
    fn status_left(&mut self, ui: &mut egui::Ui) {
        let source = match self.list.source {
            Source::Dzsa => "From the DZSA list (the list to use is in Settings)",
            Source::Steam => "From Steam's list (the list to use is in Settings)",
        };
        ui.label(format!(
            "{} of {} servers",
            self.view.len(),
            self.servers.len()
        ))
        .on_hover_text(source);
        if let Some(e) = &self.list.error {
            ui.label(RichText::new(format!("· {}", sentence(e))).color(ERROR));
        }
        let fakes = self.fakes_hidden();
        if fakes > 0 {
            ui.label(RichText::new("·").weak());
            let text = match fakes {
                1 => "1 likely fake hidden".to_string(),
                n => format!("{n} likely fakes hidden"),
            };
            if ui
                .link(RichText::new(text).weak())
                .on_hover_text(
                    "Hidden by \"Hide likely fakes\" in More filters: servers listed twice, \
                     copies of another, and ones whose numbers can't be right. Click to show them.",
                )
                .clicked()
            {
                self.state.filters.show_suspect = true;
                self.filters_changed();
            }
        }
        if self.pending_resort && self.table_hovered {
            ui.label(RichText::new("· paused while pointing").weak())
                .on_hover_text(
                    "New results wait while the pointer is on the list, so rows don't move \
                     under it",
                );
            if ui.small_button("Update now").clicked() {
                self.view_dirty = true;
            }
        }
        if self.ping_matters() && !self.pinging.is_empty() {
            ui.label(RichText::new(format!("· pinging {} servers", self.pinging.len())).weak());
        }
        if let Some(fetched) = self.list.fetched {
            ui.label(RichText::new(format!("· updated {}", ago(fetched))).weak());
        }
    }
}

fn ago(time: SystemTime) -> String {
    let secs = time.elapsed().map(|d| d.as_secs()).unwrap_or(0);
    match secs {
        0..=59 => "just now".into(),
        60..=3599 => format!("{} min ago", secs / 60),
        _ => format!("{} h ago", secs / 3600),
    }
}
