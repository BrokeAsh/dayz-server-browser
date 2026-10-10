//! DayZ news: the News button in the top bar (with a count of new posts), and the window listing
//! the developers' latest posts on Steam. The news is fetched at start and every few hours.

use eframe::egui::{self, RichText};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use super::{App, Msg, windows};
use crate::news::{self, Item};

/// Ask Steam for the news again after this long...
const FETCH_AGAIN: Duration = Duration::from_secs(6 * 60 * 60);
/// ...or this long after a failure.
const RETRY: Duration = Duration::from_secs(30 * 60);

#[derive(Default)]
pub struct News {
    items: Vec<Item>,
    loading: bool,
    error: Option<String>,
    last_fetch: Option<Instant>,
    /// Open, with the newest post seen before it opened (to mark the ones new since).
    window: Option<u64>,
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

impl App {
    /// At start and every few hours: fetches the news in the background.
    pub(super) fn news_now_and_then(&mut self) {
        let again = if self.news.error.is_some() {
            RETRY
        } else {
            FETCH_AGAIN
        };
        if !self.news.loading && self.news.last_fetch.is_none_or(|t| t.elapsed() > again) {
            self.fetch_news();
        }
    }

    fn fetch_news(&mut self) {
        self.news.loading = true;
        self.news.last_fetch = Some(Instant::now());
        self.spawn(|| Msg::News(news::fetch().map_err(|e| format!("{e:#}"))));
    }

    pub(super) fn news_msg(&mut self, result: Result<Vec<Item>, String>) {
        self.news.loading = false;
        match result {
            Ok(items) => {
                self.news.items = items;
                self.news.error = None;
                if self.news.window.is_some() {
                    self.news_seen();
                }
            }
            // Quietly: the window says so, and what was fetched before stays.
            Err(e) => {
                log::info!("couldn't get the DayZ news: {e}");
                self.news.error = Some(e);
            }
        }
    }

    /// Remembers the newest post as seen.
    fn news_seen(&mut self) {
        let newest = news::newest(&self.news.items);
        if newest > self.state.news_seen {
            self.state.news_seen = newest;
            self.state_changed();
        }
    }

    /// The top bar's News button, with how many posts are new.
    pub(super) fn news_button(&mut self, ui: &mut egui::Ui) {
        let new = news::unseen(&self.news.items, self.state.news_seen, now());
        let button = if new > 0 {
            let badge = RichText::new(format!("{new} new"))
                .small()
                .color(ui.visuals().hyperlink_color);
            egui::Button::new(("News", badge))
        } else {
            egui::Button::new("News")
        };
        let hover = match new {
            0 => "News from the DayZ developers on Steam".to_string(),
            1 => "1 new post from the DayZ developers".to_string(),
            n => format!("{n} new posts from the DayZ developers"),
        };
        if ui.add(button).on_hover_text(hover).clicked() {
            if self.news.window.is_some() {
                self.news.window = None;
            } else {
                self.news.window = Some(self.state.news_seen);
                self.news_seen();
            }
        }
    }

    pub(super) fn news_window(&mut self, ctx: &egui::Context) {
        let Some(seen_before) = self.news.window else {
            return;
        };
        let mut open = true;
        let mut retry = false;
        let mut follow = None;
        let id = egui::Id::new("news-window");
        windows::placed(egui::Window::new("DayZ news"), ctx, id, 460.0)
            .collapsible(false)
            .resizable(true)
            .default_width(460.0)
            .open(&mut open)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new("The developers' latest posts on Steam")
                            .weak()
                            .small(),
                    );
                    if self.news.loading {
                        ui.spinner();
                    }
                });
                if let Some(e) = &self.news.error {
                    ui.horizontal_wrapped(|ui| {
                        let text = if self.news.items.is_empty() {
                            "Couldn't get the news right now."
                        } else {
                            "Couldn't check for newer posts."
                        };
                        ui.label(RichText::new(text).weak())
                            .on_hover_text(super::sentence(e));
                        retry = ui
                            .add_enabled(!self.news.loading, egui::Button::new("Try again"))
                            .clicked();
                    });
                } else if self.news.items.is_empty() && !self.news.loading {
                    ui.label(RichText::new("No posts yet.").weak());
                }
                ui.add_space(4.0);
                let now = now();
                egui::ScrollArea::vertical()
                    .max_height(440.0)
                    .show(ui, |ui| {
                        for (i, item) in self.news.items.iter().enumerate() {
                            if i > 0 {
                                ui.separator();
                            }
                            let title = ui
                                .link(RichText::new(&item.title).strong())
                                .on_hover_text(&item.url);
                            if title.clicked() {
                                follow = Some(item.url.clone());
                            }
                            ui.horizontal(|ui| {
                                if news::is_new(item, seen_before, now) {
                                    ui.label(
                                        RichText::new("New")
                                            .small()
                                            .strong()
                                            .color(ui.visuals().hyperlink_color),
                                    );
                                }
                                ui.label(RichText::new(news::ago(item.date, now)).weak().small());
                            });
                            if !item.snippet.is_empty() {
                                ui.label(&item.snippet);
                            }
                        }
                    });
                ui.add_space(4.0);
                if ui.link("All DayZ news on Steam").clicked() {
                    follow = Some(news::all_news_page());
                }
            });
        if windows::escape_closes(ctx, id) {
            open = false;
        }
        if let Some(url) = follow
            && let Err(e) = open::that_detached(&url)
        {
            log::warn!("couldn't open {url}: {e}");
            self.toast("Couldn't open your web browser");
        }
        if retry {
            self.fetch_news();
        }
        if !open {
            self.news.window = None;
        }
    }
}
