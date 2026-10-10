//! The browser window: the server table with filters, a details panel for the selected server,
//! and the join and settings windows. Slow work (downloads, queries) runs on threads that report
//! back through a channel.
//!
//! This file holds the app's state, its messages, and the logic behind the views: loading the
//! list, pinging, asking servers for their mods, and building the filtered, sorted view. Each
//! part of the window is drawn in its own file: the server table in `list`, the filters panel in
//! `filters`, the details panel in `details`, the top and status bars in `bars`, and the windows
//! in `join`, `connect`, `settings` and `updates`.

mod bars;
mod connect;
mod details;
mod filters;
mod join;
mod list;
mod settings;
mod updates;

use eframe::egui::{self, Align, Color32};
use std::collections::{HashMap, HashSet};
use std::sync::mpsc;
use std::time::{Duration, Instant, SystemTime};

use crate::a2s::{self, Info, Pinger, Pong, Rules};
use crate::address::Found;
use crate::dzsa;
use crate::mods::{LocalMods, ModState};
use crate::overlay;
use crate::secret::{self, Storage};
use crate::server::{self, ModRef, Seen, Server, SortBy, map_title};
use crate::settings::{Settings, Source, State, data_dir};
use crate::steam::{self, Install};
use crate::steam_api::{self, FileDetails};
use crate::version;
use crate::workshop;

/// At start, a cached list younger than this is used instead of fetching it again.
const LIST_MAX_AGE: Duration = Duration::from_secs(5 * 60);
/// The shortest gap between two fetches the user asks for.
const REFRESH_COOLDOWN: Duration = Duration::from_secs(30);
/// Ping a visible server again after this long.
const PING_AGAIN: Duration = Duration::from_secs(5 * 60);
/// While ping results keep arriving, re-sort or re-filter at most this often.
const RESORT_EVERY: Duration = Duration::from_secs(4);
/// A server pinged again only moves in a ping sort if its ping changed by more than this (ms),
/// so ordinary jitter doesn't shuffle the list.
const PING_JITTER: u32 = 30;

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
    Update(updates::UpdateMsg),
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
    /// When DayZ was last launched, and with which mods.
    last_launch: Option<(Instant, Vec<u64>)>,
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
    update: updates::UpdateStatus,
    last_update_check: Option<Instant>,
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
            update: updates::UpdateStatus::Idle,
            last_update_check: None,
            settings,
        };
        app.load_list(false);
        app.check_overlay(true);
        app.read_game_version();
        app.check_for_update_now_and_then();
        // Leftovers of an update; a moment later, once the version it replaced has quit.
        std::thread::spawn(|| {
            std::thread::sleep(Duration::from_secs(10));
            crate::update::clean_up();
        });
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
        let Some(program) = overlay::program() else {
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
                let ping_moved = match (live.ping, pong.ping) {
                    (Some(old), Some(new)) => old.abs_diff(new) > PING_JITTER,
                    (old, new) => old != new,
                };
                live.ping = pong.ping;
                if pong.info.is_some() {
                    live.info = pong.info;
                }
                live.at = Some(Instant::now());
                let counts_changed = self.answer_matters() && counts(self) != before;
                if (self.ping_matters() && ping_moved) || counts_changed {
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
            Msg::Update(msg) => self.update_msg(msg),
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
        if then == Then::Join
            && !self
                .connect_window
                .as_ref()
                .is_some_and(|w| w.asking.as_deref() == Some(asked))
        {
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
    fn mods_for(&self, server: &Server) -> (Option<Vec<ModRef>>, &'static str) {
        match self.rules.get(&server.addr()) {
            Some(Fetch::Done(rules)) => (Some(rules.mods.clone()), "from the server"),
            _ if !server.mods.is_empty() => (Some(server.mods.clone()), "from the DZSA list"),
            _ if !server.modded => (Some(Vec::new()), "vanilla"),
            _ => (None, ""),
        }
    }

    fn mod_state(&self, m: &ModRef) -> ModState {
        self.local
            .state(m.id, self.details.get(&m.id).map(|d| d.time_updated))
    }

    fn select(&mut self, addr: &str, scroll: bool) {
        self.selected = Some(addr.to_string());
        if scroll {
            self.scroll_to_selected = Some(Align::Center);
        }
        if let Some(&i) = self.index.get(addr) {
            self.request_rules(addr);
            let (mods, _) = self.mods_for(&self.servers[i]);
            if let Some(mods) = mods {
                let ids: Vec<u64> = mods.iter().map(|m| m.id).collect();
                self.request_details(&ids);
            }
        }
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
        self.check_for_update_now_and_then();
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

/// Failures, and mods that are missing.
const ERROR: Color32 = Color32::from_rgb(230, 120, 90);
/// Warnings that don't stop anything, such as a server on another game version.
const WARN: Color32 = Color32::from_rgb(230, 170, 90);
/// Middling: a fair ping, a mod with an update.
const FAIR: Color32 = Color32::from_rgb(220, 200, 90);
const GOOD: Color32 = Color32::from_rgb(110, 200, 120);

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
