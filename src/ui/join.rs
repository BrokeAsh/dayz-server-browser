//! The join window: checks the server's mods against what's installed, gets missing ones through
//! Steam, asks for the password if there is one, and launches the game, now or once the server
//! has room.

use eframe::egui::{self, RichText};
use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use super::{App, ERROR, Fetch, GOOD, Msg, OverlayStatus, WARN, open_workshop, state_badge};
use crate::a2s::{self, Pong};
use crate::launch;
use crate::mods::ModState;
use crate::server::{ModRef, Server, map_title};
use crate::settings::{Recent, data_dir};
use crate::steam_api;
use crate::workshop::{self, Event, ItemState};

/// How often to ask the server while waiting for room: often, because after a restart the
/// slots go fast.
const WATCH_EVERY: Duration = Duration::from_secs(1);
/// Only an answer this recent counts as room to join.
const FRESH: Duration = Duration::from_secs(3);
/// Silent this long while waiting counts as a restart.
const RESTARTING: Duration = Duration::from_secs(5);
/// After the Workshop helper exits, Steam may still count DayZ as running for a moment and
/// ignore a launch; launching waits this long.
const SETTLE: Duration = Duration::from_secs(5);
/// A launch with other mods soon after another, before the game shows up, could pull the first
/// one's mod links from under it.
const RELAUNCH_GAP: Duration = Duration::from_secs(45);

pub struct Join {
    pub addr: String,
    /// The server, kept up to date from the list while it's there: a refresh that drops it (say,
    /// mid-restart) mustn't close the window.
    server: Server,
    password: String,
    run: Option<WorkshopRun>,
    message: Option<(String, bool)>,
    game_running: bool,
    game_checked: Option<Instant>,
    /// Waiting for room (a free slot and nobody queued), to join as soon as there is some.
    watch: Option<SlotWatch>,
    /// While waiting: when the server stopped answering, and whether it stayed silent long enough
    /// to count as a restart (not just a lost reply).
    silent_since: Option<Instant>,
    server_down: bool,
    /// While waiting, the mod list was asked for again (after a restart, or because the first
    /// try failed). If that fails too, the list's mods are the best there is.
    rules_retried: bool,
}

/// Asks one server for its player count and queue every second, until dropped. Its answers arrive
/// as ordinary pings, so the window and the list both see them.
struct SlotWatch {
    stop: Arc<AtomicBool>,
}

impl SlotWatch {
    fn start(app: &App, addr: &str) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let (tx, ctx, addr, stopped) = (
            app.tx.clone(),
            app.ctx.clone(),
            addr.to_string(),
            stop.clone(),
        );
        std::thread::spawn(move || {
            let Ok(target) = a2s::resolve(&addr) else {
                return;
            };
            while !stopped.load(Ordering::Relaxed) {
                let answer = a2s::info(target, Duration::from_secs(2)).ok();
                let pong = Pong {
                    addr: addr.clone(),
                    ping: answer.as_ref().map(|(_, took)| took.as_millis() as u32),
                    info: answer.map(|(info, _)| info),
                };
                if tx.send(Msg::Pong(pong)).is_err() {
                    return;
                }
                ctx.request_repaint();
                let next = Instant::now() + WATCH_EVERY;
                while Instant::now() < next && !stopped.load(Ordering::Relaxed) {
                    std::thread::sleep(Duration::from_millis(100));
                }
            }
        });
        Self { stop }
    }
}

impl Drop for SlotWatch {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// One run of the Workshop helper. Its events carry its number, so a cancelled run's late ones
/// can't land on the next.
struct WorkshopRun {
    id: u64,
    helper: workshop::Helper,
    connected: bool,
    items: BTreeMap<u64, Progress>,
    error: Option<String>,
    finished_at: Option<Instant>,
    cancelled: bool,
}

impl WorkshopRun {
    fn finished(&self) -> bool {
        self.finished_at.is_some()
    }
}

static NEXT_RUN: AtomicU64 = AtomicU64::new(1);

#[derive(Clone)]
struct Progress {
    state: ItemState,
    done: u64,
    total: u64,
    message: Option<String>,
}

impl Join {
    pub fn new(server: &Server) -> Self {
        Self {
            addr: server.addr(),
            server: server.clone(),
            password: String::new(),
            run: None,
            message: None,
            game_running: false,
            game_checked: None,
            watch: None,
            silent_since: None,
            server_down: false,
            rules_retried: false,
        }
    }

    pub fn workshop_event(&mut self, run: u64, event: Event) {
        let Some(r) = self.run.as_mut().filter(|r| r.id == run) else {
            return;
        };
        match event {
            Event::Ready => r.connected = true,
            Event::Item {
                id,
                state,
                done,
                total,
                message,
            } => {
                r.items.insert(
                    id,
                    Progress {
                        state,
                        done,
                        total,
                        message,
                    },
                );
            }
            // A cancelled helper's complaints are expected.
            Event::Error { message } if !r.cancelled => r.error = Some(message),
            Event::Error { .. } | Event::Done => {}
            Event::Exited => {
                r.finished_at.get_or_insert_with(Instant::now);
                if !r.connected && r.error.is_none() && !r.cancelled {
                    r.error = Some("The Workshop helper stopped before reaching Steam".into());
                }
            }
        }
    }

    pub(super) fn downloading(&self) -> bool {
        self.run.as_ref().is_some_and(|r| !r.finished())
    }

    /// The helper exited moments ago, so Steam may still ignore a launch.
    fn settling(&self) -> bool {
        self.run
            .as_ref()
            .and_then(|r| r.finished_at)
            .is_some_and(|t| t.elapsed() < SETTLE)
    }
}

/// What the join window shows, worked out before drawing.
struct View {
    server: Server,
    players: u32,
    slots: u32,
    /// Players queued, if any.
    queue: Option<u32>,
    /// The server's latest answer said nobody is queued.
    no_queue: bool,
    mods: Option<Vec<ModRef>>,
    origin: &'static str,
    states: Vec<(ModRef, ModState)>,
    /// Workshop mods to download or update.
    need: Vec<u64>,
    missing: usize,
    outdated: usize,
    not_workshop: usize,
    loading: bool,
    rules_done: bool,
    rules_failed: bool,
    truncated: bool,
    locked: bool,
    /// The server's version and yours, when they differ.
    other_version: Option<(String, String)>,
    install_error: Option<String>,
    helper: bool,
    has_key: bool,
    /// The server's latest answer was no answer.
    down: bool,
    /// It answered within the last few seconds.
    fresh: bool,
}

impl View {
    fn full(&self) -> bool {
        self.slots > 0 && self.players >= self.slots
    }

    fn room(&self) -> bool {
        self.fresh && self.slots > 0 && self.players < self.slots && self.no_queue
    }

    fn workshop_ids(&self) -> Vec<u64> {
        self.mods
            .iter()
            .flatten()
            .filter(|m| m.id != 0)
            .map(|m| m.id)
            .collect()
    }
}

enum Action {
    Download(Vec<u64>),
    Subscribe(Vec<u64>),
    OpenPages(Vec<u64>),
    CancelDownload,
    Launch(Vec<u64>),
    WaitForRoom,
    StopWaiting,
    Close,
}

impl App {
    pub(super) fn join_window(&mut self, ctx: &egui::Context) {
        let Some(addr) = self.join.as_ref().map(|j| j.addr.clone()) else {
            return;
        };
        if let Some(&i) = self.index.get(&addr) {
            let server = self.servers[i].clone();
            self.join.as_mut().unwrap().server = server;
        }
        self.join_checks();
        let view = self.join_view(&addr);
        let not_ready = self.not_ready(&view);
        let mut action = self.watch_step(&view, not_ready.is_none());

        let join = self.join.as_mut().unwrap();
        let mut open = true;
        egui::Window::new("Join server")
            .collapsible(false)
            .resizable(true)
            .default_width(480.0)
            .open(&mut open)
            .show(ctx, |ui| {
                let server = &view.server;
                ui.label(RichText::new(&server.name).strong().size(16.0));
                let (players, slots) = (view.players, view.slots);
                ui.label(
                    RichText::new(match view.queue {
                        Some(q) => format!("{} · {players}/{slots} players · {q} in queue", map_title(&server.map)),
                        None => format!("{} · {players}/{slots} players", map_title(&server.map)),
                    })
                    .weak(),
                );
                ui.add_space(8.0);
                mods_section(ui, &view, join, &mut action);
                download_section(ui, &view, join, &mut action);
                ui.add_space(8.0);
                if view.locked {
                    ui.horizontal(|ui| {
                        ui.label("Password");
                        ui.add(egui::TextEdit::singleline(&mut join.password).password(true).desired_width(200.0));
                    });
                }
                if let Some((theirs, yours)) = &view.other_version {
                    ui.label(
                        RichText::new(format!(
                            "This server runs DayZ {theirs}, and you have {yours}. DayZ won't join a server \
                             on another version."
                        ))
                        .color(WARN),
                    );
                }
                wait_section(ui, &view, join, not_ready.as_deref(), &mut action);
                if join.game_running {
                    ui.label(RichText::new("DayZ is already running; close it first.").color(WARN));
                }
                if let Some(e) = &view.install_error {
                    ui.label(RichText::new(e).color(ERROR));
                }
                if view.outdated > 0 && view.missing == 0 && !join.downloading() {
                    ui.label(RichText::new("Some mods have updates; joining may fail until they're updated.").weak().small());
                }
                if let Some((message, error)) = &join.message {
                    let color = if *error { ERROR } else { ui.visuals().text_color() };
                    ui.label(RichText::new(message).color(color));
                }
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    let launch = ui
                        .add_enabled(not_ready.is_none(), egui::Button::new(RichText::new("  Launch DayZ  ").strong()))
                        .on_disabled_hover_text(not_ready.as_deref().unwrap_or_default());
                    if launch.clicked() {
                        action = Some(Action::Launch(view.workshop_ids()));
                    }
                    if ui.button("Close").clicked() {
                        action = Some(Action::Close);
                    }
                });
            });
        if !open {
            action = Some(Action::Close);
        }
        if let Some(action) = action {
            self.join_action(action, &view);
        }
        if self.join.as_ref().is_some_and(Join::downloading) {
            ctx.request_repaint_after(Duration::from_millis(250));
        } else if self.join.is_some() {
            // Notice mods Steam finishes downloading on its own.
            ctx.request_repaint_after(Duration::from_secs(2));
        }
    }

    /// Re-reads the installed mods and whether the game runs, every couple of seconds.
    fn join_checks(&mut self) {
        if self.local_loaded.elapsed() > Duration::from_secs(2) {
            self.reload_local();
        }
        let join = self.join.as_mut().unwrap();
        if join
            .game_checked
            .is_none_or(|t| t.elapsed() > Duration::from_secs(2))
        {
            join.game_running = launch::game_running();
            join.game_checked = Some(Instant::now());
        }
    }

    fn join_view(&self, addr: &str) -> View {
        let join = self.join.as_ref().unwrap();
        let server = join.server.clone();
        let seen = self.seen(&server);
        let live = self.live.get(addr);
        let info = live.and_then(|l| l.info.as_ref());
        let (mods, origin) = self.mods_for(&server);
        let rules = self.rules.get(addr);
        let states: Vec<(ModRef, ModState)> = mods
            .iter()
            .flatten()
            .map(|m| (m.clone(), self.mod_state(m)))
            .collect();
        let count = |wanted: ModState| states.iter().filter(|(_, s)| *s == wanted).count();
        let (players, slots) = seen.players.unwrap_or((server.players, server.max_players));
        let server_version = info
            .map(|i| i.version.clone())
            .filter(|v| !v.is_empty())
            .unwrap_or(server.version.clone());
        View {
            players,
            slots,
            queue: seen.queue.or(server.queue).filter(|q| *q > 0),
            no_queue: seen.queue == Some(0),
            need: states
                .iter()
                .filter(|(_, s)| matches!(s, ModState::Missing | ModState::Outdated))
                .map(|(m, _)| m.id)
                .collect(),
            missing: count(ModState::Missing),
            outdated: count(ModState::Outdated),
            not_workshop: count(ModState::NotWorkshop),
            states,
            mods,
            origin,
            loading: matches!(rules, Some(Fetch::Loading)),
            rules_done: matches!(rules, Some(Fetch::Done(_))),
            rules_failed: matches!(rules, Some(Fetch::Failed(_))),
            truncated: matches!(rules, Some(Fetch::Done(r)) if r.truncated),
            locked: server.password || seen.locked,
            other_version: match (&self.game_version, seen.same_version) {
                (Some(game), Some(false)) => Some((server_version, game.clone())),
                _ => None,
            },
            install_error: self.install.as_ref().err().cloned(),
            helper: workshop::helper().is_some(),
            has_key: self.key.is_some(),
            down: live.is_some_and(|l| l.at.is_some() && l.ping.is_none()),
            fresh: live
                .is_some_and(|l| l.ping.is_some() && l.at.is_some_and(|t| t.elapsed() < FRESH)),
            server,
        }
    }

    /// Why the game can't be launched right now, if it can't.
    fn not_ready(&self, view: &View) -> Option<String> {
        let join = self.join.as_ref()?;
        Some(if let Some(e) = &view.install_error {
            e.clone()
        } else if view.loading {
            "Getting the server's mod list…".into()
        } else if view.mods.is_none() {
            "The server's mod list isn't known".into()
        } else if view.missing > 0 {
            format!("{} mods to download first", view.missing)
        } else if join.downloading() {
            "Downloads are still running".into()
        } else if join.settling() {
            "Steam is finishing the downloads…".into()
        } else if join.game_running {
            "DayZ is already running".into()
        } else {
            return None;
        })
    }

    /// While waiting for room: notices restarts, asks for the mod list again when it may have
    /// changed, and launches once there's room and everything's ready.
    fn watch_step(&mut self, view: &View, ready: bool) -> Option<Action> {
        let join = self.join.as_mut()?;
        join.watch.as_ref()?;
        if view.down {
            let since = *join.silent_since.get_or_insert_with(Instant::now);
            if since.elapsed() > RESTARTING && !join.server_down {
                join.server_down = true;
                join.rules_retried = false;
            }
            return None;
        }
        if !view.fresh {
            return None;
        }
        join.silent_since = None;
        // Back after a restart (its mods may have changed), or the list couldn't be read: ask
        // once more before joining.
        let back = std::mem::take(&mut join.server_down);
        if back || (view.rules_failed && !join.rules_retried) {
            join.rules_retried = true;
            self.rules.remove(&join.addr);
            let addr = join.addr.clone();
            self.request_rules(&addr);
            return None;
        }
        let mods_known = view.rules_done || (view.rules_failed && join.rules_retried);
        let password_ok = !view.locked || !join.password.is_empty();
        (view.room() && ready && mods_known && password_ok)
            .then(|| Action::Launch(view.workshop_ids()))
    }

    fn join_action(&mut self, action: Action, view: &View) {
        match action {
            // Closing stops the helper and the waiting; Steam keeps downloading anything already
            // subscribed.
            Action::Close => self.join = None,
            Action::OpenPages(ids) => {
                for id in ids.iter().take(15) {
                    open_workshop(*id);
                }
                if ids.len() > 15 {
                    self.toast(format!("Opened the first 15 of {} pages", ids.len()));
                }
            }
            Action::Download(ids) => {
                let id = NEXT_RUN.fetch_add(1, Ordering::Relaxed);
                let tx = self.tx.clone();
                let ctx = self.ctx.clone();
                let started = workshop::Helper::start(&ids, move |event| {
                    let _ = tx.send(Msg::Workshop(id, event));
                    ctx.request_repaint();
                });
                let join = self.join.as_mut().unwrap();
                match started {
                    Ok(helper) => {
                        join.message = None;
                        join.run = Some(WorkshopRun {
                            id,
                            helper,
                            connected: false,
                            items: BTreeMap::new(),
                            error: None,
                            finished_at: None,
                            cancelled: false,
                        });
                    }
                    Err(e) => join.message = Some((format!("{e:#}"), true)),
                }
            }
            Action::CancelDownload => {
                if let Some(run) = self.join.as_mut().and_then(|j| j.run.as_mut()) {
                    run.helper.cancel();
                    run.cancelled = true;
                    run.finished_at = Some(Instant::now());
                }
            }
            Action::WaitForRoom => {
                let watch = SlotWatch::start(self, &view.server.addr());
                if let Some(join) = self.join.as_mut() {
                    join.message = None;
                    join.watch = Some(watch);
                }
            }
            Action::StopWaiting => {
                if let Some(join) = self.join.as_mut() {
                    join.watch = None;
                    join.silent_since = None;
                    join.server_down = false;
                }
            }
            Action::Subscribe(ids) => {
                let Some((key, _)) = self.key.clone() else {
                    return;
                };
                self.spawn(move || {
                    let failed = ids
                        .iter()
                        .filter(|id| steam_api::subscribe(&key, **id).is_err())
                        .count();
                    Msg::Toast(if failed == 0 {
                        format!(
                            "Subscribed to {} mods; Steam is downloading them",
                            ids.len()
                        )
                    } else {
                        format!("{failed} of {} subscriptions failed", ids.len())
                    })
                });
            }
            Action::Launch(ids) => match self.launch(view, &ids) {
                Ok(()) => {
                    let waited = self.join.take().is_some_and(|j| j.watch.is_some());
                    self.toast(if waited {
                        "There's room: starting DayZ… (the browser stays open)"
                    } else {
                        "Starting DayZ… (the browser stays open)"
                    });
                }
                Err(e) => {
                    if let Some(join) = self.join.as_mut() {
                        join.message = Some((format!("{e:#}"), true));
                        // Don't keep trying a launch that fails.
                        join.watch = None;
                    }
                }
            },
        }
    }

    fn launch(&mut self, view: &View, ids: &[u64]) -> anyhow::Result<()> {
        let other_mods =
            |(at, last): &(Instant, Vec<u64>)| at.elapsed() < RELAUNCH_GAP && last != ids;
        if self.last_launch.as_ref().is_some_and(other_mods) && !launch::game_running() {
            anyhow::bail!(
                "DayZ is still starting from the last launch, with other mods; give it a moment"
            );
        }
        let install = self.install.as_ref().map_err(|e| anyhow::anyhow!("{e}"))?;
        let extra = launch::parse_extra(&self.settings.extra_args)?;
        let password = self
            .join
            .as_ref()
            .map(|j| j.password.clone())
            .filter(|p| view.locked && !p.is_empty());
        let plan = launch::build(
            install,
            &view.server,
            ids,
            password.as_deref(),
            &extra,
            &data_dir(),
        )?;
        log::info!("launching: {}", plan.display());
        if self.settings.overlay && self.overlay != OverlayStatus::Running {
            self.check_overlay(true);
        }
        plan.spawn()?;
        self.last_launch = Some((Instant::now(), ids.to_vec()));
        let joined = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        self.state.add_recent(Recent {
            addr: view.server.addr(),
            name: view.server.name.clone(),
            map: view.server.map.clone(),
            joined,
        });
        self.state_changed();
        Ok(())
    }
}

/// The server's mods and how each stands.
fn mods_section(ui: &mut egui::Ui, view: &View, join: &Join, action: &mut Option<Action>) {
    if view.loading && view.mods.is_none() {
        ui.horizontal(|ui| {
            ui.spinner();
            ui.label("Asking the server for its mod list…");
        });
        return;
    }
    if view.mods.is_none() {
        ui.label(
            RichText::new("The server didn't send its mod list, so it can't be joined from here.")
                .color(ERROR),
        );
        return;
    }
    if view.states.is_empty() {
        ui.label("No mods needed.");
        return;
    }
    ui.horizontal(|ui| {
        ui.label(RichText::new(format!("{} mods", view.states.len())).strong());
        if view.loading {
            ui.spinner();
        } else {
            ui.label(RichText::new(view.origin).weak().small());
        }
    });
    egui::ScrollArea::vertical()
        .max_height(260.0)
        .id_salt("join-mods")
        .show(ui, |ui| {
            egui::Grid::new("join-mod-grid")
                .num_columns(3)
                .spacing([8.0, 3.0])
                .striped(true)
                .show(ui, |ui| {
                    for (m, state) in &view.states {
                        let (icon, color, tip) = state_badge(*state);
                        ui.label(RichText::new(icon).color(color))
                            .on_hover_text(tip);
                        ui.label(&m.name);
                        mod_cell(ui, m, *state, join.run.as_ref(), action);
                        ui.end_row();
                    }
                });
        });
    if view.truncated {
        ui.label(
            RichText::new("The server's list was cut short; it may need more mods than shown.")
                .weak()
                .small(),
        );
    }
    if view.not_workshop > 0 {
        ui.label(
            RichText::new(format!(
                "{} mod(s) aren't on the Steam Workshop; the server has to provide them.",
                view.not_workshop
            ))
            .weak()
            .small(),
        );
    }
}

/// A mod row's last cell: download progress while a download runs, else a way to get it.
fn mod_cell(
    ui: &mut egui::Ui,
    m: &ModRef,
    state: ModState,
    run: Option<&WorkshopRun>,
    action: &mut Option<Action>,
) {
    if matches!(state, ModState::Installed | ModState::NotWorkshop) {
        ui.label("");
        return;
    }
    let progress = run.and_then(|r| r.items.get(&m.id));
    if let Some(p) = progress
        && run.is_some_and(|r| !r.finished())
    {
        progress_cell(ui, p);
        return;
    }
    ui.horizontal(|ui| {
        if let Some(p) = progress.filter(|p| p.state == ItemState::Failed) {
            let failed = ui.label(RichText::new("failed").color(ERROR));
            if let Some(message) = &p.message {
                failed.on_hover_text(message);
            }
        }
        if ui.small_button("Workshop page").clicked() {
            *action = Some(Action::OpenPages(vec![m.id]));
        }
    });
}

fn progress_cell(ui: &mut egui::Ui, p: &Progress) {
    match p.state {
        ItemState::Subscribing => {
            ui.label(RichText::new("subscribing…").weak());
        }
        ItemState::Queued => {
            ui.label(RichText::new("queued").weak());
        }
        ItemState::Downloading => {
            let fraction = if p.total > 0 {
                p.done as f32 / p.total as f32
            } else {
                0.0
            };
            ui.add(
                egui::ProgressBar::new(fraction)
                    .desired_width(130.0)
                    .text(format!("{} / {}", megabytes(p.done), megabytes(p.total))),
            );
        }
        // Steam says it's done, but the mod doesn't look current yet.
        ItemState::Installed => {
            ui.label(RichText::new("finishing…").color(GOOD));
        }
        ItemState::Failed => {
            let label = ui.label(RichText::new("failed").color(ERROR));
            if let Some(m) = &p.message {
                label.on_hover_text(m);
            }
        }
    }
}

/// Getting the missing and outdated mods.
fn download_section(ui: &mut egui::Ui, view: &View, join: &Join, action: &mut Option<Action>) {
    if !view.need.is_empty() {
        ui.add_space(8.0);
        let mut parts = Vec::new();
        if view.missing > 0 {
            parts.push(format!("{} to download", view.missing));
        }
        if view.outdated > 0 {
            parts.push(format!("{} to update", view.outdated));
        }
        ui.label(RichText::new(parts.join(", ")).strong());
        ui.horizontal_wrapped(|ui| {
            if view.helper {
                let busy = join.downloading();
                if ui
                    .add_enabled(!busy, egui::Button::new("⬇ Download with Steam"))
                    .on_hover_text(
                        "Subscribes and downloads through your Steam client. Steam shows DayZ as \
                         running until it's done.",
                    )
                    .clicked()
                {
                    *action = Some(Action::Download(view.need.clone()));
                }
                if busy && ui.button("Cancel").clicked() {
                    *action = Some(Action::CancelDownload);
                }
            } else if view.has_key && ui.button("Subscribe with my Steam key").clicked() {
                *action = Some(Action::Subscribe(view.need.clone()));
            }
            if ui
                .button("Open Workshop pages")
                .on_hover_text(
                    "Subscribe in Steam yourself; this window notices when they're downloaded",
                )
                .clicked()
            {
                *action = Some(Action::OpenPages(view.need.clone()));
            }
        });
    }
    let Some(run) = &join.run else { return };
    if let Some(e) = &run.error {
        ui.label(RichText::new(e).color(ERROR));
    } else if run.cancelled {
        ui.label(RichText::new("Download cancelled").weak());
    } else if !run.finished() && !run.connected {
        ui.horizontal(|ui| {
            ui.spinner();
            ui.label("Connecting to Steam…");
        });
    } else if !run.finished() {
        ui.label(
            RichText::new("Downloading through Steam (it shows DayZ as running until done)…")
                .weak(),
        );
    }
}

/// When joining now wouldn't get straight in (full, queued, or restarting): says so, and offers
/// to wait for room.
fn wait_section(
    ui: &mut egui::Ui,
    view: &View,
    join: &Join,
    not_ready: Option<&str>,
    action: &mut Option<Action>,
) {
    let waiting = join.watch.is_some();
    if !(view.full() || view.queue.is_some() || view.down || waiting) {
        return;
    }
    let situation = if view.down {
        "The server isn't answering; it may be restarting.".to_string()
    } else {
        match (view.full(), view.queue) {
            (true, Some(q)) => {
                format!("The server is full, with {q} waiting: joining now puts you in its queue.")
            }
            (true, None) => {
                "The server is full: DayZ may queue you or turn you away to the main menu.".into()
            }
            (false, Some(q)) => {
                format!("{q} waiting in the queue: joining now puts you behind them.")
            }
            (false, None) => String::new(),
        }
    };
    if !situation.is_empty() {
        ui.label(RichText::new(situation).color(WARN));
    }
    let needs_password = view.locked && join.password.is_empty();
    ui.horizontal(|ui| {
        if waiting {
            ui.spinner();
            match not_ready {
                Some(reason) => {
                    ui.label(RichText::new(format!("Waiting, but: {reason}")).color(WARN))
                }
                None if needs_password => {
                    ui.label(RichText::new("Waiting, but: type the password first").color(WARN))
                }
                None if view.down => ui.label("Waiting for it to come back with room…"),
                None => ui.label("Waiting for a free slot with no queue…"),
            };
            if ui.button("Stop").clicked() {
                *action = Some(Action::StopWaiting);
            }
            return;
        }
        let why_not = not_ready.or(needs_password.then_some("Type the password first"));
        if ui
            .add_enabled(
                why_not.is_none(),
                egui::Button::new("Join when there's room"),
            )
            .on_hover_text(
                "Asks the server every second and starts DayZ as soon as it has a free slot and \
                 nobody in its queue, such as right after a restart, so you don't wait in the \
                 queue. Keep this window open.",
            )
            .on_disabled_hover_text(why_not.unwrap_or_default())
            .clicked()
        {
            *action = Some(Action::WaitForRoom);
        }
    });
    if waiting {
        ui.label(
            RichText::new(
                "DayZ starts by itself when there's room; the mod list is checked again after a restart.",
            )
            .weak()
            .small(),
        );
    }
}

fn megabytes(bytes: u64) -> String {
    format!("{:.0} MB", bytes as f64 / 1_000_000.0)
}
