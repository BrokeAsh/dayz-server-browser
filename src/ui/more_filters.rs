//! The "More filters" section of the filters panel (country, official or community, in-game day
//! or night, time speed, BattlEye, low population, likely fakes), and what the details panel and
//! the settings show about countries, time speed, and likely fakes.

use eframe::egui::{self, RichText};

use super::screening::CountryChoice;
use super::{App, WARN, filters::range};
use crate::a2s::Tags;
use crate::geo;
use crate::server::{Daytime, Kind, Server};

impl App {
    /// The "More filters" section, folded away until opened. Returns whether a filter changed.
    pub(super) fn more_filters(&mut self, ui: &mut egui::Ui) -> bool {
        let f = &self.state.filters;
        // How many differ from the defaults, so a folded section still shows it's doing something.
        let set = [
            !f.countries.is_empty(),
            f.kind != Kind::Any,
            f.daytime != Daytime::Any,
            f.min_speed > 0 || f.max_speed > 0,
            f.battleye_only,
            f.hide_low_population,
            f.show_suspect,
        ]
        .into_iter()
        .filter(|on| *on)
        .count();
        let title = match set {
            0 => "More filters".to_string(),
            n => format!("More filters ({n})"),
        };
        let mut changed = false;
        egui::CollapsingHeader::new(title)
            .id_salt("more filters")
            .default_open(set > 0)
            .show(ui, |ui| {
                changed |= self.country_filter(ui);
                let f = &mut self.state.filters;
                ui.add_space(4.0);
                ui.label("Servers");
                ui.horizontal(|ui| {
                    changed |= ui.selectable_value(&mut f.kind, Kind::Any, "Any").changed();
                    changed |= ui
                        .selectable_value(&mut f.kind, Kind::Official, "Official")
                        .on_hover_text("The game's own servers")
                        .changed();
                    changed |= ui
                        .selectable_value(&mut f.kind, Kind::Community, "Community")
                        .changed();
                });
                ui.label("In-game time");
                ui.horizontal(|ui| {
                    changed |= ui
                        .selectable_value(&mut f.daytime, Daytime::Any, "Any")
                        .changed();
                    changed |= ui
                        .selectable_value(&mut f.daytime, Daytime::Day, "Day")
                        .on_hover_text("06:00 to 17:59 on the server")
                        .changed();
                    changed |= ui
                        .selectable_value(&mut f.daytime, Daytime::Night, "Night")
                        .on_hover_text("18:00 to 05:59 on the server")
                        .changed();
                });
                // On its own line: indented here, the label and both limits don't fit in one.
                ui.label("Time speed")
                    .on_hover_text("How many times faster than real time the in-game day runs");
                changed |= range(ui, &mut f.min_speed, &mut f.max_speed);
                ui.add_space(4.0);
                changed |= ui.checkbox(&mut f.battleye_only, "BattlEye only").changed();
                changed |= ui
                    .checkbox(&mut f.hide_low_population, "Hide low population")
                    .on_hover_text("Servers less than 30% full")
                    .changed();
                let mut hide = !f.show_suspect;
                if ui
                    .checkbox(&mut hide, "Hide likely fakes")
                    .on_hover_text(
                        "Servers listed twice, copies of another server, and servers whose \
                         numbers can't be right. Favorites, recent servers, and servers added \
                         by address always show.",
                    )
                    .changed()
                {
                    f.show_suspect = !hide;
                    changed = true;
                }
            });
        changed
    }

    /// The country filter: a list of the countries servers are in, with a search, to tick any
    /// number of. Returns whether the pick changed.
    fn country_filter(&mut self, ui: &mut egui::Ui) -> bool {
        let mut changed = false;
        let f = &mut self.state.filters;
        let screen = &mut self.screen;
        let title = |code: &str| geo::name(code).unwrap_or(code).to_string();
        let current = match f.countries.as_slice() {
            [] => "Any country".to_string(),
            [code] => title(code),
            [code, rest @ ..] => format!("{} +{}", title(code), rest.len()),
        };
        let popup = egui::ComboBox::from_id_salt("country")
            .selected_text(current)
            .width(170.0)
            .height(400.0)
            .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
            .show_ui(ui, |ui| {
                let search = ui.add(
                    egui::TextEdit::singleline(&mut screen.country_query)
                        .hint_text("Type to search")
                        .desired_width(f32::INFINITY),
                );
                if !screen.country_popup_open {
                    search.request_focus();
                }
                let query = screen.country_query.trim().to_lowercase();
                let matches = |c: &CountryChoice| {
                    c.code.to_lowercase().starts_with(&query)
                        || geo::name(&c.code).is_some_and(|n| n.to_lowercase().contains(&query))
                };
                // Picked countries stay on top (with no servers if none is there now); the
                // rest, busiest first, narrowed by the search.
                let mut rows: Vec<CountryChoice> = f
                    .countries
                    .iter()
                    .map(|code| {
                        screen
                            .choices
                            .iter()
                            .find(|c| c.code == *code)
                            .cloned()
                            .unwrap_or_else(|| CountryChoice {
                                code: code.clone(),
                                servers: 0,
                            })
                    })
                    .collect();
                rows.extend(
                    screen
                        .choices
                        .iter()
                        .filter(|c| !f.countries.contains(&c.code) && matches(c))
                        .cloned(),
                );
                // Enter picks the first match that isn't picked yet.
                if search.lost_focus()
                    && ui.input(|i| i.key_pressed(egui::Key::Enter))
                    && let Some(row) = rows.get(f.countries.len())
                {
                    f.countries.push(row.code.clone());
                    screen.country_query.clear();
                    changed = true;
                    search.request_focus();
                }
                if ui
                    .add_enabled(!f.countries.is_empty(), egui::Button::new("Any country"))
                    .clicked()
                {
                    f.countries.clear();
                    changed = true;
                }
                ui.separator();
                egui::ScrollArea::vertical()
                    .max_height(320.0)
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        if screen.geo.is_none() {
                            ui.label(
                                RichText::new(
                                    "Countries show once the country database is downloaded.",
                                )
                                .weak(),
                            );
                        } else if rows.is_empty() {
                            ui.label(RichText::new("No countries match").weak());
                        }
                        for row in rows {
                            let mut on = f.countries.contains(&row.code);
                            let label = format!("{} ({})", title(&row.code), row.servers);
                            if ui
                                .checkbox(&mut on, label)
                                .on_hover_text(&row.code)
                                .changed()
                            {
                                if on {
                                    f.countries.push(row.code);
                                } else {
                                    f.countries.retain(|c| *c != row.code);
                                }
                                changed = true;
                            }
                        }
                    });
            });
        let open = popup.inner.is_some();
        if open && !screen.country_popup_open {
            // Fresh search each time it opens.
            screen.country_query.clear();
        }
        screen.country_popup_open = open;
        changed
    }

    /// Rows for the details grid: the server's country, whether it's official, and how fast its
    /// time runs (from its answer's tags if it answered, else the list).
    pub(super) fn more_details(&self, ui: &mut egui::Ui, s: &Server, tags: Option<&Tags>) {
        let row = |ui: &mut egui::Ui, k: &str, v: String| {
            ui.label(RichText::new(k).weak());
            ui.label(v);
            ui.end_row();
        };
        if let Some(code) = &s.country {
            row(ui, "Country", geo::describe(code));
        }
        row(
            ui,
            "Type",
            if s.official { "Official" } else { "Community" }.into(),
        );
        let day = tags.and_then(|t| t.day_speed).or(s.day_speed);
        let night = tags.and_then(|t| t.night_speed()).or(s.night_speed);
        if let Some(text) = time_speed(day, night) {
            row(ui, "Time speed", text);
        }
    }

    /// For a server that looks fake or like a second listing: why, and a way to the one shown
    /// instead.
    pub(super) fn suspect_note(&mut self, ui: &mut egui::Ui, addr: &str) {
        let Some(suspect) = self.suspect(addr) else {
            return;
        };
        let text = suspect.explain(&self.servers);
        let other = suspect
            .other()
            .and_then(|i| self.servers.get(i))
            .map(|s| s.addr());
        ui.add_space(6.0);
        ui.label(RichText::new(text).color(WARN));
        if let Some(other) = other
            && ui
                .small_button("Show the other listing")
                .on_hover_text("Select the listing that's shown instead")
                .clicked()
        {
            self.select(&other, true);
        }
    }

    /// The credits line for the country data, which its licence (CC BY 4.0) asks for.
    pub(super) fn country_credits(&self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("Server countries:").weak().small());
            ui.hyperlink_to(
                RichText::new("IP geolocation by DB-IP").small(),
                "https://db-ip.com",
            );
            let when = match self.country_database_month() {
                Some((year, month)) => format!("(database of {year}-{month:02})"),
                None => "(not downloaded yet)".into(),
            };
            ui.label(RichText::new(when).weak().small());
        });
    }
}

/// "12× by day, 48× at night", or just the day's speed when that's all that's known.
fn time_speed(day: Option<f32>, night: Option<f32>) -> Option<String> {
    let day = day?;
    Some(match night {
        Some(night) => format!("{} by day, {} at night", times(day), times(night)),
        None => times(day),
    })
}

/// A speed such as `12×`, `5.4×`, or `0.1×`.
fn times(speed: f32) -> String {
    if (speed - speed.round()).abs() < 0.05 || speed >= 100.0 {
        format!("{:.0}×", speed.round())
    } else {
        format!("{speed:.1}×")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn time_speeds() {
        assert_eq!(time_speed(None, Some(4.0)), None);
        assert_eq!(time_speed(Some(12.0), None).as_deref(), Some("12×"));
        assert_eq!(
            time_speed(Some(12.0), Some(576.0)).as_deref(),
            Some("12× by day, 576× at night")
        );
        assert_eq!(
            time_speed(Some(5.4), Some(11.718)).as_deref(),
            Some("5.4× by day, 11.7× at night")
        );
        assert_eq!(times(0.1), "0.1×");
        assert_eq!(times(3.999), "4×");
    }
}
