//! The filters panel on the left: the map filter with its search, the checkboxes, perspective
//! and mods, the player, size and ping limits, Reset, and the Recent servers.

use eframe::egui::{self, RichText};

use super::{App, MapChoice};
use crate::server::{Filters, ModFilter, Perspective, map_title};

impl App {
    pub(super) fn filters_panel(&mut self, ui: &mut egui::Ui) {
        let mut changed = false;
        let f = &mut self.state.filters;
        ui.heading("Filters");
        ui.add_space(4.0);
        // Picked maps no server runs right now still show, with no servers.
        let choice = |key: &String| {
            self.maps
                .iter()
                .find(|c| c.key == *key)
                .cloned()
                .unwrap_or_else(|| MapChoice {
                    key: key.clone(),
                    title: map_title(key),
                    servers: 0,
                })
        };
        let current = match f.maps.as_slice() {
            [] => "Any map".to_string(),
            [map] => choice(map).title,
            [map, rest @ ..] => format!("{} +{}", choice(map).title, rest.len()),
        };
        let popup = egui::ComboBox::from_id_salt("map")
            .selected_text(current)
            .width(170.0)
            .height(400.0)
            .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
            .show_ui(ui, |ui| {
                let search = ui.add(
                    egui::TextEdit::singleline(&mut self.map_query)
                        .hint_text("Type to search")
                        .desired_width(f32::INFINITY),
                );
                if !self.map_popup_open {
                    search.request_focus();
                }
                let query = self.map_query.trim().to_lowercase();
                // Picked maps stay on top; the rest, busiest first, narrowed by the search.
                let mut rows: Vec<MapChoice> = f.maps.iter().map(choice).collect();
                rows.extend(
                    self.maps
                        .iter()
                        .filter(|c| !f.maps.contains(&c.key) && c.key.contains(&query))
                        .cloned(),
                );
                // Enter picks the first match that isn't picked yet.
                if search.lost_focus()
                    && ui.input(|i| i.key_pressed(egui::Key::Enter))
                    && let Some(row) = rows.get(f.maps.len())
                {
                    f.maps.push(row.key.clone());
                    self.map_query.clear();
                    changed = true;
                    search.request_focus();
                }
                if ui
                    .add_enabled(!f.maps.is_empty(), egui::Button::new("Any map"))
                    .clicked()
                {
                    f.maps.clear();
                    changed = true;
                }
                ui.separator();
                egui::ScrollArea::vertical()
                    .max_height(320.0)
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        if rows.is_empty() {
                            ui.label(RichText::new("No maps match").weak());
                        }
                        for row in rows {
                            let mut on = f.maps.contains(&row.key);
                            let label = format!("{} ({})", row.title, row.servers);
                            if ui.checkbox(&mut on, label).changed() {
                                if on {
                                    f.maps.push(row.key);
                                } else {
                                    f.maps.retain(|m| *m != row.key);
                                }
                                changed = true;
                            }
                        }
                    });
            });
        let open = popup.inner.is_some();
        if open && !self.map_popup_open {
            // Fresh search each time it opens.
            self.map_query.clear();
        }
        self.map_popup_open = open;
        ui.add_space(4.0);
        changed |= ui.checkbox(&mut f.hide_empty, "Hide empty").changed();
        changed |= ui.checkbox(&mut f.hide_full, "Hide full").changed();
        changed |= ui.checkbox(&mut f.hide_password, "Hide locked").changed();
        changed |= ui
            .checkbox(&mut f.favorites_only, "Favorites only")
            .changed();
        let version_hint = format!(
            "Servers known to run another version than yours ({})",
            self.game_version.as_deref().unwrap_or_default()
        );
        changed |= ui
            .add_enabled(
                self.game_version.is_some(),
                egui::Checkbox::new(&mut f.hide_other_versions, "Hide other versions"),
            )
            .on_hover_text(version_hint)
            .on_disabled_hover_text("Your DayZ version isn't known (is the DayZ folder found?)")
            .changed();
        ui.add_space(6.0);
        ui.label("Perspective");
        ui.horizontal(|ui| {
            changed |= ui
                .selectable_value(&mut f.perspective, Perspective::Any, "Any")
                .changed();
            changed |= ui
                .selectable_value(&mut f.perspective, Perspective::First, "1PP")
                .changed();
            changed |= ui
                .selectable_value(&mut f.perspective, Perspective::Third, "3PP")
                .changed();
        });
        ui.label("Mods");
        ui.horizontal(|ui| {
            changed |= ui
                .selectable_value(&mut f.mods, ModFilter::Any, "Any")
                .changed();
            changed |= ui
                .selectable_value(&mut f.mods, ModFilter::Vanilla, "Vanilla")
                .changed();
            changed |= ui
                .selectable_value(&mut f.mods, ModFilter::Modded, "Modded")
                .changed();
        });
        ui.add_space(6.0);
        egui::Grid::new("limits")
            .num_columns(2)
            .spacing([6.0, 4.0])
            .show(ui, |ui| {
                ui.label("Players");
                changed |= range(ui, &mut f.min_players, &mut f.max_players);
                ui.end_row();
                ui.label("Server size");
                changed |= range(ui, &mut f.min_slots, &mut f.max_slots);
                ui.end_row();
                ui.label("Max ping");
                changed |= ui
                    .add(
                        egui::DragValue::new(&mut f.max_ping)
                            .range(0..=999)
                            .speed(5)
                            .custom_formatter(|v, _| {
                                if v == 0.0 {
                                    "any".into()
                                } else {
                                    format!("{v:.0} ms")
                                }
                            }),
                    )
                    .changed();
                ui.end_row();
            });
        ui.add_space(6.0);
        if ui.button("Reset filters").clicked() {
            *f = Filters {
                search: std::mem::take(&mut f.search),
                ..Default::default()
            };
            changed = true;
        }
        if changed {
            self.filters_changed();
        }

        ui.add_space(12.0);
        ui.heading("Recent");
        if self.state.recent.is_empty() {
            ui.label(RichText::new("Servers you join appear here.").weak());
        }
        let mut pick = None;
        for recent in self.state.recent.iter().take(8) {
            let text = format!(
                "{} · {}",
                truncate(&recent.name, 26),
                map_title(&recent.map)
            );
            if ui
                .selectable_label(self.selected.as_deref() == Some(&recent.addr), text)
                .on_hover_text(&recent.name)
                .clicked()
            {
                pick = Some(recent.addr.clone());
            }
        }
        if let Some(addr) = pick {
            self.select(&addr, true);
        }
    }
}

/// A "from … to …" pair of limits, where 0 means no limit. Moving one past the other moves both.
fn range(ui: &mut egui::Ui, min: &mut u32, max: &mut u32) -> bool {
    let any = |v: f64, _| {
        if v == 0.0 {
            "any".to_string()
        } else {
            format!("{v:.0}")
        }
    };
    let max_was_any = *max == 0;
    ui.horizontal(|ui| {
        let low = ui.add(
            egui::DragValue::new(min)
                .range(0..=500)
                .speed(0.5)
                .custom_formatter(any),
        );
        ui.label("to");
        let high = ui.add(
            egui::DragValue::new(max)
                .range(0..=500)
                .speed(0.5)
                .custom_formatter(any),
        );
        if *max != 0 && *min > *max {
            // A maximum that was "any" starts from the minimum.
            if low.changed() || max_was_any {
                *max = *min;
            } else {
                *min = *max;
            }
        }
        low.changed() || high.changed()
    })
    .inner
}

pub(super) fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_string()
    } else {
        let cut: String = text.chars().take(max - 1).collect();
        format!("{cut}…")
    }
}
