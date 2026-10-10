//! What the list knows beyond each server's own listing: which country it's in, from DB-IP's
//! database (read at start and downloaded again monthly, in the background, with the lookups
//! on a thread too), and whether it looks fake or like a second listing (`suspect`'s rules, run
//! again as answers come in). The filters, the Country column, and the details panel read both.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::{App, Msg};
use crate::a2s;
use crate::geo::{self, Geo};
use crate::server::Server;
use crate::settings::data_dir;
use crate::suspect::{self, Answer, Disagreements, Groups, Suspect};

/// Answers are compared with the list's player counts only while it's this fresh; an older
/// list's counts may simply be out of date.
const FRESH_LIST: Duration = Duration::from_secs(10 * 60);

pub enum ScreenMsg {
    /// The saved country database, if there's a readable one, and whether it's due for a new one.
    Saved(Option<Arc<Geo>>, bool),
    Downloaded(Result<Arc<Geo>, String>),
    /// Countries looked up (by IP) with the database of this generation.
    Countries(u32, Vec<(String, Option<String>)>),
}

/// One entry in the country filter: a country and how many servers are in it.
#[derive(Clone)]
pub(super) struct CountryChoice {
    pub code: String,
    pub servers: usize,
}

#[derive(Default)]
pub(super) struct Screening {
    pub geo: Option<Arc<Geo>>,
    /// Bumped with each database, so lookups made with an older one are dropped.
    generation: u32,
    /// Countries by IP, as looked up so far: `None` for an IP the database doesn't place.
    countries: HashMap<String, Option<String>>,
    looking_up: bool,
    /// The countries servers are in, busiest first.
    pub choices: Vec<CountryChoice>,
    groups: Groups,
    /// Goes with `App::servers`.
    pub flags: Vec<Option<Suspect>>,
    disagreements: Disagreements,
    /// The country filter's search text, and whether its list was open last frame.
    pub country_query: String,
    pub country_popup_open: bool,
}

impl App {
    /// At start: reads the saved country database, in the background (it's 8 MB).
    pub(super) fn start_screening(&mut self) {
        let dir = data_dir();
        self.spawn(move || {
            let (geo, stale) = geo::load(&dir);
            Msg::Screen(ScreenMsg::Saved(geo.map(Arc::new), stale))
        });
    }

    pub(super) fn screen_msg(&mut self, msg: ScreenMsg) {
        match msg {
            ScreenMsg::Saved(geo, stale) => {
                if let Some(geo) = geo {
                    self.use_geo(geo);
                }
                if stale {
                    let dir = data_dir();
                    self.spawn(move || {
                        Msg::Screen(ScreenMsg::Downloaded(
                            geo::download(&dir)
                                .map(Arc::new)
                                .map_err(|e| format!("{e:#}")),
                        ))
                    });
                }
            }
            ScreenMsg::Downloaded(Ok(geo)) => {
                log::info!("downloaded DB-IP's country database");
                self.use_geo(geo);
            }
            // The saved one (if any) stays; it's tried again at the next start.
            ScreenMsg::Downloaded(Err(e)) => {
                log::warn!("couldn't update the country database: {e}")
            }
            ScreenMsg::Countries(generation, found) => {
                self.screen.looking_up = false;
                if generation == self.screen.generation {
                    self.screen.countries.extend(found);
                    self.fill_countries();
                }
                // Servers that came in meanwhile, or all of them for a newer database.
                self.look_up_countries();
            }
        }
    }

    /// Starts using a country database: every server is looked up again, since a newer one may
    /// place some elsewhere. Countries shown so far stay until then.
    fn use_geo(&mut self, geo: Arc<Geo>) {
        self.screen.geo = Some(geo);
        self.screen.generation += 1;
        self.screen.countries.clear();
        self.look_up_countries();
    }

    /// Looks up, on a thread, the servers whose country isn't known yet.
    fn look_up_countries(&mut self) {
        let Some(geo) = self.screen.geo.clone() else {
            return;
        };
        if self.screen.looking_up {
            return;
        }
        let mut ips: HashSet<String> = HashSet::new();
        for s in &self.servers {
            if !self.screen.countries.contains_key(&s.ip) {
                ips.insert(s.ip.clone());
            }
        }
        if ips.is_empty() {
            return;
        }
        self.screen.looking_up = true;
        let generation = self.screen.generation;
        self.spawn(move || {
            let found = ips
                .into_iter()
                .map(|ip| {
                    let country = geo.country(&ip);
                    (ip, country)
                })
                .collect();
            Msg::Screen(ScreenMsg::Countries(generation, found))
        });
    }

    /// Gives the servers the countries looked up so far.
    fn fill_countries(&mut self) {
        let countries = &self.screen.countries;
        let fill = |s: &mut Server| {
            if let Some(country) = countries.get(&s.ip) {
                s.country.clone_from(country);
            }
        };
        self.servers.iter_mut().for_each(fill);
        // Servers added by address go into each new list from here.
        self.added.iter_mut().for_each(fill);
        let mut counts: HashMap<&str, usize> = HashMap::new();
        for code in self.servers.iter().filter_map(|s| s.country.as_deref()) {
            *counts.entry(code).or_default() += 1;
        }
        let mut choices: Vec<CountryChoice> = counts
            .into_iter()
            .map(|(code, servers)| CountryChoice {
                code: code.to_string(),
                servers,
            })
            .collect();
        choices.sort_by(|a, b| b.servers.cmp(&a.servers).then_with(|| a.code.cmp(&b.code)));
        self.screen.choices = choices;
        self.view_dirty = true;
    }

    /// After the list changed: countries from what's known, and the groups the duplicate rules
    /// look at.
    pub(super) fn screen_servers(&mut self) {
        self.fill_countries();
        self.screen.groups = Groups::new(&self.servers);
        // The old flags point at the old list's servers.
        self.screen.flags = vec![None; self.servers.len()];
        self.look_up_countries();
    }

    /// After a listed server answered: notes whether its count disagrees with the list's.
    pub(super) fn screen_answer(&mut self, addr: &str) {
        let fresh = self
            .list
            .fetched
            .is_some_and(|t| t.elapsed().unwrap_or(Duration::MAX) < FRESH_LIST);
        let Some(&i) = self.index.get(addr) else {
            return;
        };
        // A server added by address is listed as it answered.
        if !fresh || i >= self.listed {
            return;
        }
        if let Some(info) = self.live.get(addr).and_then(|l| l.info.as_ref()) {
            self.screen.disagreements.answer(
                addr,
                self.servers[i].players,
                info.players,
                Instant::now(),
            );
        }
    }

    /// Runs the likely-fake rules over the list with the latest answers, before the view is
    /// built.
    pub(super) fn screen_flags(&mut self) {
        let answers: Vec<Answer> = self
            .servers
            .iter()
            .map(|s| {
                let addr = s.addr();
                let live = self.live.get(&addr);
                let info = live.and_then(|l| l.info.as_ref());
                Answer {
                    silent: live.is_some_and(|l| l.at.is_some() && l.ping.is_none()),
                    players: info.map(|i| (i.players, i.max_players)),
                    // An answer without a queue tag means nobody is queued.
                    queue: info.map(|i| a2s::parse_tags(&i.keywords).queue.unwrap_or(0)),
                    disagrees: self.screen.disagreements.confirmed(&addr, s.players),
                }
            })
            .collect();
        self.screen.flags = suspect::flag(&self.servers, &answers, &self.screen.groups);
    }

    /// Why a server looks fake or like a second listing, if it does.
    pub(super) fn suspect(&self, addr: &str) -> Option<&Suspect> {
        let &i = self.index.get(addr)?;
        self.screen.flags.get(i)?.as_ref()
    }

    /// The user's own servers, which the likely-fakes filter never hides: favorites, recent
    /// ones, servers added by address, and watched ones.
    pub(super) fn keeps(&self, addr: &str) -> bool {
        self.state.is_favorite(addr)
            || self.state.recent.iter().any(|r| r.addr == addr)
            || self.state.added.iter().any(|a| a == addr)
            || self.state.is_watched(addr)
    }

    /// Whether the likely-fakes filter hides a server.
    pub(super) fn hidden_as_suspect(&self, addr: &str) -> bool {
        self.suspect(addr).is_some() && !self.keeps(addr)
    }

    /// When DB-IP made the country database in use (year and month), for the credits.
    pub(super) fn country_database_month(&self) -> Option<(i32, u32)> {
        let built = self.screen.geo.as_ref()?.built()?;
        Some(geo::year_month(built))
    }
}
