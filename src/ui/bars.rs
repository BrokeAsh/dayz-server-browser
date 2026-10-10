//! The bars around the list: the top bar (Refresh, Connect by address, Play offline, the search,
//! Settings, News, and the Update button) and the status bar (list counts and age, pinging, the
//! map overlay, DayZ).

use eframe::egui::{self, Align, Layout, RichText};
use std::time::SystemTime;

use super::{App, ERROR, GOOD, OverlayStatus, WARN, connect, settings};
use crate::settings::Source;

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
            let search = ui.add(
                egui::TextEdit::singleline(&mut self.state.filters.search)
                    .hint_text("name, map, mod, or address")
                    .desired_width(260.0),
            );
            if search.changed() {
                self.filters_changed();
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.button("⚙ Settings").clicked() && self.settings_window.is_none() {
                    self.settings_window = Some(settings::SettingsWindow::new(&self.settings));
                }
                self.news_button(ui);
                self.update_button(ui);
                let source = match self.list.source {
                    Source::Dzsa => "DZSA list",
                    Source::Steam => "Steam list",
                };
                ui.label(RichText::new(source).weak());
            });
        });
    }

    pub(super) fn status_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(format!(
                "{} of {} servers",
                self.view.len(),
                self.servers.len()
            ));
            if let Some(fetched) = self.list.fetched {
                ui.label(RichText::new(format!("· updated {}", ago(fetched))).weak());
            }
            if let Some(e) = &self.list.error {
                ui.label(RichText::new(format!("· {e}")).color(ERROR));
            }
            if self.ping_matters() && !self.pinging.is_empty() {
                ui.label(RichText::new(format!("· pinging {} servers", self.pinging.len())).weak());
            }
            if self.pending_resort && self.table_hovered {
                ui.label(
                    RichText::new("· new results wait while the pointer is on the list").weak(),
                );
                if ui.small_button("Update now").clicked() {
                    self.view_dirty = true;
                }
            }
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
                ui.separator();
                match &self.install {
                    Ok(_) => ui.label(RichText::new("DayZ found").weak()),
                    Err(e) => ui.label(RichText::new(e).color(ERROR)),
                };
                self.watch_button(ui);
            });
        });
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
