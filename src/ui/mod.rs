//! The browser window: the server table with filters, a details panel for the selected server,
//! and the join and settings windows. Slow work (downloads, queries) runs on threads that report
//! back through a channel.

mod connect;
mod join;
mod settings;

use eframe::egui::{self, Align, Color32, Layout, RichText, Sense};
use egui_extras::{Column, TableBuilder};
use std::collections::{HashMap, HashSet};
use std::sync::mpsc;
use std::time::{Duration, Instant, SystemTime};

use crate::a2s::{self, Info, Pinger, Pong, Rules};
use crate::address::Found;
use crate::dzsa;
use crate::mods::{LocalMods, ModState};
use crate::overlay;
use crate::secret::{self, Storage};
use crate::server::{
    self, Filters, ModFilter, ModRef, Perspective, Seen, Server, SortBy, map_title,
};
use crate::settings::{COLUMNS, Settings, Source, State, data_dir};
use crate::steam::{self, Install};
use crate::steam_api::{self, FileDetails};
use crate::version;
use crate::workshop;

/// At start, a cached list younger than this is used instead of fetching it again.
const LIST_MAX_AGE: Duration = Duration::from_secs(5 * 60);
/// The shortest gap between two fetches the user asks for.
const REFRESH_COOLDOWN: Duration = Duration::from_secs(30);
/// Ping a visible server again after this long.
const PING_AGAIN: Duration = Duration::from_secs(120);
/// While ping results keep arriving, re-sort or re-filter at most this often.
const RESORT_EVERY: Duration = Duration::from_secs(1);

pub enum Msg {
    List {
        source: Source,
        result: Result<Vec<Server>, String>,
        fetched: SystemTime,
        cached: bool,
    },
    Pong(Pong),
    Rules(String, Result<Rules, String>),
    Details(Vec<u64>, Result<HashMap<u64, FileDetails>, String>),
    KeyTest(Result<Storage, String>),
    /// An event from the Workshop helper run with this number.
    Workshop(u64, workshop::Event),
    Overlay(OverlayStatus),
    Toast(String),
    /// The installed game's version.
    GameVersion(Result<String, String>),
    /// A server looked up by address, and what to do with it.
    Found {
        addr: String,
        result: Result<Found, String>,
        then: Then,
    },
}

/// What a server looked up by address is for.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Then {
    /// One added earlier, looked up again at start.
    Keep,
    /// Typed into the Connect window: remember it, and open the join window.
    Join,
    /// Picked from Recent but missing from the list: show it.
    Show,
}

#[derive(Debug, Clone, PartialEq)]
pub enum OverlayStatus {
    Off,
    Checking,
    NotFound,
    Running,
    Failed(String),
}

enum Fetch<T> {
    Loading,
    Done(T),
    Failed(String),
}

#[derive(Default)]
struct Live {
    ping: Option<u32>,
    info: Option<Info>,
    at: Option<Instant>,
}

struct ListStatus {
    source: Source,
    fetched: Option<SystemTime>,
    loading: bool,
    error: Option<String>,
    last_request: Option<Instant>,
}

/// One entry in the map filter: the worlds sharing a display name, and how many servers run them.
#[derive(Clone)]
struct MapChoice {
    key: String,
    title: String,
    servers: usize,
}

pub struct App {
    ctx: egui::Context,
    tx: mpsc::Sender<Msg>,
    rx: mpsc::Receiver<Msg>,
    settings: Settings,
    state: State,
    state_changed: Option<Instant>,
    install: Result<Install, String>,
    local: LocalMods,
    local_loaded: Instant,
    key: Option<(String, Storage)>,
    game_version: Option<String>,

    servers: Vec<Server>,
    /// Servers looked up by address that answered. The ones the fetched list lacks go after it in
    /// `servers`, which holds `listed` fetched servers first.
    added: Vec<Server>,
    listed: usize,
    /// A server being looked up from Recent, and the last such lookup that failed.
    looking_up: Option<String>,
    lookup_failed: Option<(String, String)>,
    /// When DayZ was last launched.
    last_launch: Option<Instant>,
    index: HashMap<String, usize>,
    list: ListStatus,
    view: Vec<usize>,
    view_dirty: bool,
    maps: Vec<MapChoice>,
    /// The map filter's search text, and whether its list was open last frame.
    map_query: String,
    map_popup_open: bool,
    selected: Option<String>,
    /// Scroll the table to the selected server, placing it like this.
    scroll_to_selected: Option<Align>,
    /// The servers on screen in the last frame, top to bottom.
    on_screen: Vec<String>,

    pinger: Pinger,
    live: HashMap<String, Live>,
    /// Servers waiting for a ping, and whether each was asked for ahead of the queue.
    pinging: HashMap<String, bool>,
    /// Ping results that would move rows arrived; applied when the pointer isn't on the list.
    pending_resort: bool,
    last_rebuild: Instant,
    table_hovered: bool,
    rules: HashMap<String, Fetch<Rules>>,
    details: HashMap<u64, FileDetails>,
    details_pending: HashSet<u64>,

    overlay: OverlayStatus,
    join: Option<join::Join>,
    connect_window: Option<connect::ConnectWindow>,
    settings_window: Option<settings::SettingsWindow>,
    toast: Option<(String, Instant)>,
}

impl App {
    pub fn new(cc: &eframe::CreationContext) -> Self {
        let ctx = cc.egui_ctx.clone();
        // Rows are clicked to select them, so labels mustn't take clicks for text selection.
        ctx.global_style_mut(|style| style.interaction.selectable_labels = false);
        let (tx, rx) = mpsc::channel();
        let pinger = {
            let tx = tx.clone();
            let ctx = ctx.clone();
            Pinger::spawn(move |pong| {
                let _ = tx.send(Msg::Pong(pong));
                ctx.request_repaint();
            })
        };
        let settings = Settings::load();
        let install = steam::find(settings.game_dir.as_deref()).map_err(|e| format!("{e:#}"));
        let local = install.as_ref().map(LocalMods::load).unwrap_or_default();
        let mut app = Self {
            ctx,
            tx,
            rx,
            state: State::load(),
            state_changed: None,
            install,
            local,
            local_loaded: Instant::now(),
            key: secret::load(),
            game_version: None,
            servers: Vec::new(),
            added: Vec::new(),
            listed: 0,
            looking_up: None,
            lookup_failed: None,
            last_launch: None,
            index: HashMap::new(),
            list: ListStatus {
                source: settings.source,
                fetched: None,
                loading: false,
                error: None,
                last_request: None,
            },
            view: Vec::new(),
            view_dirty: true,
            maps: Vec::new(),
            map_query: String::new(),
            map_popup_open: false,
            selected: None,
            scroll_to_selected: None,
            on_screen: Vec::new(),
            pinger,
            live: HashMap::new(),
            pinging: HashMap::new(),
            pending_resort: false,
            last_rebuild: Instant::now(),
            table_hovered: false,
            rules: HashMap::new(),
            details: HashMap::new(),
            details_pending: HashSet::new(),
            overlay: OverlayStatus::Off,
            join: None,
            connect_window: None,
            settings_window: None,
            toast: None,
            settings,
        };
        app.load_list(false);
        app.check_overlay(true);
        app.read_game_version();
        for addr in app.state.added.clone() {
            app.look_up(addr, Then::Keep);
        }
        app
    }

    /// Reads the installed game's version, in the background (it's in a 17 MB program file).
    fn read_game_version(&mut self) {
        let Ok(install) = &self.install else {
            self.game_version = None;
            return;
        };
        let game = install.game.clone();
        self.spawn(move || {
            Msg::GameVersion(version::installed(&game).map_err(|e| format!("{e:#}")))
        });
    }

    /// What's known about a server beyond the list: its latest answer, and the user's marks.
    fn seen(&self, s: &Server) -> Seen {
        let addr = s.addr();
        let live = self.live.get(&addr);
        let info = live.and_then(|l| l.info.as_ref());
        let version = info
            .map(|i| i.version.as_str())
            .filter(|v| !v.is_empty())
            .unwrap_or(&s.version);
        let tags = info.map(|i| a2s::parse_tags(&i.keywords));
        Seen {
            ping: live.and_then(|l| l.ping),
            players: info.map(|i| (i.players, i.max_players)),
            // An answer without a queue tag means nobody is queued.
            queue: tags.as_ref().map(|t| t.queue.unwrap_or(0)),
            time: tags.and_then(|t| t.time),
            locked: info.is_some_and(|i| i.password),
            favorite: self.state.is_favorite(&addr),
            same_version: self
                .game_version
                .as_deref()
                .and_then(|game| version::same(game, version)),
        }
    }

    /// Runs `work` on a thread and delivers its message.
    fn spawn(&self, work: impl FnOnce() -> Msg + Send + 'static) {
        let tx = self.tx.clone();
        let ctx = self.ctx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(work());
            ctx.request_repaint();
        });
    }

    fn toast(&mut self, message: impl Into<String>) {
        self.toast = Some((message.into(), Instant::now()));
    }

    fn state_changed(&mut self) {
        self.state_changed = Some(Instant::now());
    }

    /// Loads the server list: DZSA's from the cache while it's fresh, unless `force` (the Refresh
    /// button); a fresh download otherwise, falling back to the old cache when offline.
    fn load_list(&mut self, force: bool) {
        let source = self.settings.source;
        self.list.source = source;
        self.list.error = None;
        match source {
            Source::Dzsa => {
                self.list.loading = true;
                self.list.last_request = Some(Instant::now());
                let dir = data_dir();
                self.spawn(move || {
                    if !force
                        && let Some((servers, fetched)) = dzsa::load_cache(&dir, Some(LIST_MAX_AGE))
                    {
                        return Msg::List {
                            source,
                            result: Ok(servers),
                            fetched,
                            cached: true,
                        };
                    }
                    let result = dzsa::fetch(&dir).map_err(|e| format!("{e:#}"));
                    let result = match result {
                        Err(e) => match dzsa::load_cache(&dir, None) {
                            // Offline: show the old list and say so.
                            Some((servers, fetched)) if !force => {
                                return Msg::List {
                                    source,
                                    result: Ok(servers),
                                    fetched,
                                    cached: true,
                                };
                            }
                            _ => Err(e),
                        },
                        ok => ok,
                    };
                    Msg::List {
                        source,
                        result,
                        fetched: SystemTime::now(),
                        cached: false,
                    }
                });
            }
            Source::Steam => {
                let Some((key, _)) = self.key.clone() else {
                    self.list.loading = false;
                    self.list.error =
                        Some("Steam's list needs your Steam Web API key (Settings)".into());
                    return;
                };
                self.list.loading = true;
                self.list.last_request = Some(Instant::now());
                self.spawn(move || Msg::List {
                    source,
                    result: steam_api::server_list(&key).map_err(|e| format!("{e:#}")),
                    fetched: SystemTime::now(),
                    cached: false,
                });
            }
        }
    }

    fn refresh_allowed(&self) -> bool {
        !self.list.loading
            && self
                .list
                .last_request
                .is_none_or(|t| t.elapsed() > REFRESH_COOLDOWN)
    }

    fn check_overlay(&mut self, start: bool) {
        if !self.settings.overlay {
            self.overlay = OverlayStatus::Off;
            return;
        }
        let Some(program) = overlay::program(&self.settings) else {
            self.overlay = OverlayStatus::NotFound;
            return;
        };
        self.overlay = OverlayStatus::Checking;
        self.spawn(move || {
            if overlay::is_running(&program) {
                return Msg::Overlay(OverlayStatus::Running);
            }
            if !start {
                return Msg::Overlay(OverlayStatus::Failed("not running".into()));
            }
            if let Err(e) = overlay::start(&program) {
                return Msg::Overlay(OverlayStatus::Failed(format!("{e:#}")));
            }
            // Give it a moment to come up before asking again.
            for _ in 0..20 {
                std::thread::sleep(Duration::from_millis(250));
                if overlay::is_running(&program) {
                    return Msg::Overlay(OverlayStatus::Running);
                }
            }
            Msg::Overlay(OverlayStatus::Failed(
                "started, but it didn't answer".into(),
            ))
        });
    }

    fn handle(&mut self, msg: Msg) {
        match msg {
            Msg::List {
                source,
                result,
                fetched,
                cached,
            } => {
                if source != self.settings.source {
                    return; // The user switched lists meanwhile.
                }
                self.list.loading = false;
                match result {
                    Ok(servers) => {
                        self.set_servers(servers);
                        self.list.fetched = Some(fetched);
                        // A fresh cached list wasn't a request to DZSA: Refresh stays allowed.
                        if cached && fetched.elapsed().unwrap_or(Duration::MAX) < LIST_MAX_AGE {
                            self.list.last_request = None;
                        }
                        self.list.error = None;
                        if cached && fetched.elapsed().unwrap_or(Duration::MAX) > LIST_MAX_AGE {
                            self.list.error =
                                Some("couldn't refresh; showing the saved list".into());
                        }
                    }
                    Err(e) => self.list.error = Some(e),
                }
            }
            Msg::Pong(pong) => {
                self.pinging.remove(&pong.addr);
                // What the filters and sorting read from an answer, besides the ping.
                let index = self.index.get(&pong.addr).copied();
                let counts = |app: &Self| {
                    let s = &app.servers[index?];
                    let seen = app.seen(s);
                    Some((
                        seen.players.unwrap_or((s.players, s.max_players)),
                        seen.queue.or(s.queue),
                        seen.same_version,
                        seen.locked,
                    ))
                };
                let before = counts(self);
                let live = self.live.entry(pong.addr.clone()).or_default();
                live.ping = pong.ping;
                if pong.info.is_some() {
                    live.info = pong.info;
                }
                live.at = Some(Instant::now());
                let counts_changed = self.answer_matters() && counts(self) != before;
                if self.ping_matters() || counts_changed {
                    // Not straight away: rows moving under the pointer make the list unusable.
                    self.pending_resort = true;
                }
            }
            Msg::Rules(addr, result) => {
                let ids: Vec<u64> = match &result {
                    Ok(rules) => rules.mods.iter().map(|m| m.id).collect(),
                    Err(_) => Vec::new(),
                };
                self.rules.insert(
                    addr,
                    match result {
                        Ok(rules) => Fetch::Done(rules),
                        Err(e) => Fetch::Failed(e),
                    },
                );
                self.request_details(&ids);
            }
            Msg::Details(ids, result) => {
                for id in &ids {
                    self.details_pending.remove(id);
                }
                match result {
                    Ok(found) => self.details.extend(found),
                    Err(e) => log::debug!("Workshop details: {e}"),
                }
            }
            Msg::KeyTest(result) => {
                if let Some(window) = &mut self.settings_window {
                    window.key_result(&result);
                }
                if result.is_ok() {
                    self.key = secret::load();
                    if self.settings.source == Source::Steam {
                        self.load_list(false);
                    }
                }
            }
            Msg::Workshop(run, event) => {
                // Mods change on disk when one finishes, or when the helper is done.
                let installed = matches!(
                    event,
                    workshop::Event::Item {
                        state: workshop::ItemState::Installed,
                        ..
                    } | workshop::Event::Exited
                );
                if let Some(join) = &mut self.join {
                    join.workshop_event(run, event);
                }
                if !installed {
                    return;
                }
                self.reload_local();
            }
            Msg::Overlay(status) => self.overlay = status,
            Msg::Toast(message) => self.toast(message),
            Msg::GameVersion(result) => {
                match &result {
                    Ok(v) => log::info!("installed DayZ: {v}"),
                    Err(e) => log::warn!("couldn't read the game's version: {e}"),
                }
                self.game_version = result.ok();
                self.view_dirty = true;
            }
            Msg::Found { addr, result, then } => self.found(&addr, result, then),
        }
    }

    /// After a filter, the search, or the sort changed: rebuild the list, save, and ping what the
    /// new view needs.
    fn filters_changed(&mut self) {
        self.view_dirty = true;
        self.state_changed();
        self.ping_sweep();
    }

    fn toggle_favorite(&mut self, addr: &str) {
        self.state.toggle_favorite(addr);
        self.view_dirty |= self.state.filters.favorites_only || self.state.sort == SortBy::Favorite;
        self.state_changed();
    }

    /// Asks a server whose query address (`ip:query_port`) is known for its details.
    fn look_up(&self, addr: String, then: Then) {
        self.spawn(move || Msg::Found {
            result: crate::address::query(&addr).map_err(|e| format!("{e:#}")),
            addr,
            then,
        });
    }

    /// Whether a server is in the list only because it was added by address.
    fn only_added(&self, addr: &str) -> bool {
        self.index.get(addr).is_some_and(|&i| i >= self.listed)
    }

    /// Takes a server added by address off the list and forgets it.
    fn forget_added(&mut self, addr: &str) {
        self.state.added.retain(|a| a != addr);
        self.state_changed();
        if self.only_added(addr) && self.selected.as_deref() == Some(addr) {
            self.selected = None;
        }
        self.added.retain(|s| s.addr() != addr);
        self.reapply_added();
    }

    /// Puts the servers added by address after the fetched list again, after `added` changed.
    fn reapply_added(&mut self) {
        let mut servers = std::mem::take(&mut self.servers);
        servers.truncate(self.listed);
        self.set_servers(servers);
    }

    /// A server looked up by address answered (or didn't).
    fn found(&mut self, asked: &str, result: Result<Found, String>, mut then: Then) {
        // The Connect window was closed (or is on another address) meanwhile: just add it.
        if then == Then::Join && !self.connect_window.as_ref().is_some_and(|w| w.busy) {
            then = Then::Keep;
        }
        if then == Then::Show && self.looking_up.as_deref() == Some(asked) {
            self.looking_up = None;
        }
        let found = match result {
            Ok(found) => found,
            Err(e) => {
                match then {
                    Then::Join => {
                        if let Some(window) = &mut self.connect_window {
                            window.failed(e);
                        }
                    }
                    Then::Show => self.lookup_failed = Some((asked.to_string(), e)),
                    Then::Keep => {
                        log::info!("server {asked} (added by address) didn't answer: {e}")
                    }
                }
                return;
            }
        };
        let addr = found.server.addr();
        let live = self.live.entry(addr.clone()).or_default();
        live.ping = Some(found.ping);
        live.info = Some(found.info);
        live.at = Some(Instant::now());
        let listed = self.index.get(&addr).is_some_and(|&i| i < self.listed);
        if !listed {
            self.added.retain(|s| s.addr() != addr);
            self.added.push(found.server);
            self.reapply_added();
        }
        match then {
            Then::Keep => {}
            Then::Show => self.select(&addr, true),
            Then::Join => {
                // Remember servers the list doesn't have, newest first.
                if !listed {
                    self.state.added.retain(|a| *a != addr);
                    self.state.added.insert(0, addr.clone());
                    self.state.added.truncate(30);
                    self.state_changed();
                }
                self.connect_window = None;
                self.select(&addr, true);
                self.open_join(&addr);
            }
        }
    }

    /// Puts in a fetched list. Servers added by address that it lacks go after it.
    fn set_servers(&mut self, mut servers: Vec<Server>) {
        self.listed = servers.len();
        let listed: HashSet<String> = servers.iter().map(Server::addr).collect();
        for added in &self.added {
            if !listed.contains(&added.addr()) {
                servers.push(added.clone());
            }
        }
        self.servers = servers;
        // The old view's indexes don't apply to the new list.
        self.view.clear();
        self.index = self
            .servers
            .iter()
            .enumerate()
            .map(|(i, s)| (s.addr(), i))
            .collect();
        let mut counts: HashMap<String, MapChoice> = HashMap::new();
        for s in &self.servers {
            let key = server::map_key(&s.map);
            counts
                .entry(key.clone())
                .or_insert_with(|| MapChoice {
                    key,
                    title: map_title(&s.map),
                    servers: 0,
                })
                .servers += 1;
        }
        let mut maps: Vec<MapChoice> = counts.into_values().collect();
        maps.sort_by(|a, b| b.servers.cmp(&a.servers).then_with(|| a.key.cmp(&b.key)));
        self.maps = maps;
        self.view_dirty = true;
        self.ping_sweep();
    }

    /// Whether pings change which servers show or their order.
    fn ping_matters(&self) -> bool {
        self.state.filters.max_ping > 0 || self.state.sort == SortBy::Ping
    }

    /// Whether the list's order or filtering depends on what servers say when pinged (player
    /// counts, queue, version, password).
    fn answer_matters(&self) -> bool {
        let f = &self.state.filters;
        matches!(self.state.sort, SortBy::Players | SortBy::Queue)
            || f.hide_other_versions
            || f.hide_password
            || f.hide_empty
            || f.hide_full
            || f.min_players > 0
            || f.max_players > 0
            || f.min_slots > 0
            || f.max_slots > 0
    }

    /// When sorting or filtering by ping, pings every server that passes the other filters (in
    /// the background, behind the ones on screen), so the order covers the whole list.
    fn ping_sweep(&mut self) {
        if !self.ping_matters() {
            return;
        }
        for s in &self.servers {
            let addr = s.addr();
            let fresh = self
                .live
                .get(&addr)
                .and_then(|l| l.at)
                .is_some_and(|t| t.elapsed() < PING_AGAIN);
            if fresh
                || self.pinging.contains_key(&addr)
                || !self.state.filters.matches(
                    s,
                    &Seen {
                        ping: None,
                        ..self.seen(s)
                    },
                )
            {
                continue;
            }
            self.pinger.request_later(addr.clone());
            self.pinging.insert(addr, false);
        }
    }

    fn reload_local(&mut self) {
        if let Ok(install) = &self.install {
            self.local = LocalMods::load(install);
        }
        self.local_loaded = Instant::now();
    }

    fn request_details(&mut self, ids: &[u64]) {
        let wanted: Vec<u64> = ids
            .iter()
            .copied()
            .filter(|id| {
                *id != 0 && !self.details.contains_key(id) && !self.details_pending.contains(id)
            })
            .collect();
        if wanted.is_empty() {
            return;
        }
        self.details_pending.extend(&wanted);
        self.spawn(move || {
            let result = steam_api::file_details(&wanted).map_err(|e| format!("{e:#}"));
            Msg::Details(wanted, result)
        });
    }

    fn request_rules(&mut self, addr: &str) {
        if matches!(self.rules.get(addr), Some(Fetch::Loading | Fetch::Done(_))) {
            return;
        }
        self.rules.insert(addr.to_string(), Fetch::Loading);
        let addr = addr.to_string();
        self.spawn(move || {
            let result = a2s::resolve(&addr)
                .and_then(|target| a2s::rules(target, Duration::from_secs(3)))
                .map_err(|e| format!("{e:#}"));
            Msg::Rules(addr, result)
        });
    }

    fn rebuild_view(&mut self) {
        let filters = &self.state.filters;
        let seen: Vec<Seen> = self.servers.iter().map(|s| self.seen(s)).collect();
        let mut view: Vec<usize> = self
            .servers
            .iter()
            .enumerate()
            .filter(|(i, s)| filters.matches(s, &seen[*i]))
            .map(|(i, _)| i)
            .collect();
        let by = self.state.sort;
        view.sort_by(|&a, &b| {
            let (x, y) = (&seen[a], &seen[b]);
            let (a, b) = (&self.servers[a], &self.servers[b]);
            let order = server::compare(by, a, x, b, y);
            // Servers without a ping stay last either way.
            let unknown_ping = by == SortBy::Ping && x.ping.is_none() != y.ping.is_none();
            let order = if self.state.sort_descending && !unknown_ping {
                order.reverse()
            } else {
                order
            };
            // Ties: busiest first, then by name, so the order is stable.
            let players = |s: &Server, seen: &Seen| seen.players.map_or(s.players, |(p, _)| p);
            order
                .then_with(|| players(b, y).cmp(&players(a, x)))
                .then_with(|| a.name.cmp(&b.name))
        });
        self.view = view;
        self.view_dirty = false;
        self.pending_resort = false;
        self.last_rebuild = Instant::now();
    }

    /// The mods to use for a server: what the server itself just said, else the list's.
    fn mods_for(&self, addr: &str) -> (Option<Vec<ModRef>>, &'static str) {
        match self.rules.get(addr) {
            Some(Fetch::Done(rules)) => (Some(rules.mods.clone()), "from the server"),
            _ => {
                let server = self.index.get(addr).map(|&i| &self.servers[i]);
                match server {
                    Some(s) if !s.mods.is_empty() => (Some(s.mods.clone()), "from the DZSA list"),
                    Some(s) if !s.modded => (Some(Vec::new()), "vanilla"),
                    _ => (None, ""),
                }
            }
        }
    }

    fn mod_state(&self, m: &ModRef) -> ModState {
        self.local
            .state(m.id, self.details.get(&m.id).map(|d| d.time_updated))
    }

    fn top_bar(&mut self, ui: &mut egui::Ui) {
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
                let source = match self.list.source {
                    Source::Dzsa => "DZSA list",
                    Source::Steam => "Steam list",
                };
                ui.label(RichText::new(source).weak());
            });
        });
    }

    fn filters_panel(&mut self, ui: &mut egui::Ui) {
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

    fn select(&mut self, addr: &str, scroll: bool) {
        self.selected = Some(addr.to_string());
        if scroll {
            self.scroll_to_selected = Some(Align::Center);
        }
        if self.index.contains_key(addr) {
            self.request_rules(addr);
            let (mods, _) = self.mods_for(addr);
            if let Some(mods) = mods {
                let ids: Vec<u64> = mods.iter().map(|m| m.id).collect();
                self.request_details(&ids);
            }
        }
    }

    fn table(&mut self, ui: &mut egui::Ui) {
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
    }

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

    fn details_panel(&mut self, ui: &mut egui::Ui) {
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
        let (mods, origin) = self.mods_for(&addr);
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

    fn status_bar(&mut self, ui: &mut egui::Ui) {
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
                    OverlayStatus::NotFound => ("Map overlay not installed".into(), Some(WARN)),
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
            });
        });
    }

    fn open_join(&mut self, addr: &str) {
        let Some(&i) = self.index.get(addr) else {
            return;
        };
        if self.selected.as_deref() != Some(addr) {
            self.select(addr, false);
        }
        // A fresh mod list for joining, even if one was fetched earlier.
        if !matches!(self.rules.get(addr), Some(Fetch::Loading)) {
            self.rules.remove(addr);
            self.request_rules(addr);
        }
        self.reload_local();
        // DayZ may have updated since the browser started.
        self.read_game_version();
        self.join = Some(join::Join::new(&self.servers[i]));
    }
}

impl eframe::App for App {
    fn logic(&mut self, _ctx: &egui::Context, _frame: &mut eframe::Frame) {
        while let Ok(msg) = self.rx.try_recv() {
            self.handle(msg);
        }
        if let Some(changed) = self.state_changed
            && changed.elapsed() > Duration::from_secs(1)
        {
            self.state_changed = None;
            if let Err(e) = self.state.save() {
                log::warn!("couldn't save the browser state: {e:#}");
            }
        }
    }

    fn on_exit(&mut self) {
        // Keep a change made in the last moment before closing.
        if self.state_changed.take().is_some()
            && let Err(e) = self.state.save()
        {
            log::warn!("couldn't save the browser state: {e:#}");
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        egui::Panel::top("top").show(ui, |ui| {
            ui.add_space(4.0);
            self.top_bar(ui);
            ui.add_space(2.0);
        });
        egui::Panel::bottom("status").show(ui, |ui| self.status_bar(ui));
        egui::Panel::left("filters")
            .resizable(false)
            .exact_size(210.0)
            .show(ui, |ui| {
                egui::ScrollArea::vertical()
                    .auto_shrink([false, true])
                    .show(ui, |ui| self.filters_panel(ui));
            });
        egui::Panel::right("details")
            .resizable(true)
            .default_size(340.0)
            .min_size(260.0)
            .show(ui, |ui| self.details_panel(ui));
        egui::CentralPanel::default().show(ui, |ui| {
            self.table_hovered = ui.rect_contains_pointer(ui.max_rect());
            if self.pending_resort
                && !self.table_hovered
                && self.last_rebuild.elapsed() > RESORT_EVERY
            {
                self.view_dirty = true;
            }
            if self.servers.is_empty() {
                ui.add_space(40.0);
                ui.vertical_centered(|ui| {
                    if self.list.loading {
                        ui.spinner();
                        ui.label("Getting the server list…");
                    } else if let Some(e) = &self.list.error {
                        ui.label(e);
                    }
                });
            } else {
                self.table(ui);
            }
        });

        self.join_window(&ctx);
        self.connect_window(&ctx);
        self.settings_window(&ctx);

        if let Some((message, at)) = &self.toast {
            if at.elapsed() > Duration::from_secs(4) {
                self.toast = None;
            } else {
                egui::Area::new(egui::Id::new("toast"))
                    .anchor(egui::Align2::CENTER_BOTTOM, [0.0, -36.0])
                    .show(&ctx, |ui| {
                        egui::Frame::popup(ui.style()).show(ui, |ui| ui.label(message));
                    });
                ctx.request_repaint_after(Duration::from_millis(500));
            }
        }
        // Keep "updated N minutes ago" and pings ticking.
        ctx.request_repaint_after(Duration::from_secs(5));
    }
}

impl App {
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

/// Failures, and mods that are missing.
const ERROR: Color32 = Color32::from_rgb(230, 120, 90);
/// Warnings that don't stop anything, such as a server on another game version.
const WARN: Color32 = Color32::from_rgb(230, 170, 90);
/// Middling: a fair ping, a mod with an update.
const FAIR: Color32 = Color32::from_rgb(220, 200, 90);
const GOOD: Color32 = Color32::from_rgb(110, 200, 120);

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

fn state_badge(state: ModState) -> (&'static str, Color32, &'static str) {
    match state {
        ModState::Installed => ("✔", GOOD, "Installed"),
        ModState::Outdated => ("⟳", FAIR, "Installed, but there's an update"),
        ModState::Missing => ("⬇", ERROR, "Not downloaded"),
        ModState::NotWorkshop => ("?", Color32::GRAY, "Not on the Steam Workshop"),
    }
}

pub fn open_workshop(id: u64) {
    let url = format!("steam://url/CommunityFilePage/{id}");
    if let Err(e) = open::that_detached(&url) {
        log::warn!("couldn't open {url}: {e}");
        let _ = open::that_detached(format!(
            "https://steamcommunity.com/sharedfiles/filedetails/?id={id}"
        ));
    }
}

fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_string()
    } else {
        let cut: String = text.chars().take(max - 1).collect();
        format!("{cut}…")
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
