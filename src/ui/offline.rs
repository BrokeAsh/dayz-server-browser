//! The "Play offline" window: gets the Community Offline Mode missions from GitHub (and their
//! updates), lists the maps with their saves, and starts DayZ on one, with no server.

use eframe::egui::{self, RichText};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use super::join::RELAUNCH_GAP;
use super::{App, ERROR, Msg, OverlayStatus, WARN, open_workshop};
use crate::launch;
use crate::mods::ModState;
use crate::offline::{self, Mission, Release};
use crate::settings::data_dir;
use crate::workshop::{self, Event, ItemState};

/// While the browser stays open, ask GitHub about a new release again after this long.
const CHECK_AGAIN: Duration = Duration::from_secs(60 * 60);
/// After the Workshop helper exits, Steam may still count DayZ as running for a moment and
/// ignore a launch.
const SETTLE: Duration = Duration::from_secs(5);

pub enum OfflineMsg {
    Latest(Result<Release, String>),
    /// Download progress: bytes so far, and the total.
    Progress(u64, u64),
    Downloaded(Result<(), String>),
    /// A map's save was reset (the map's name).
    Reset(String, Result<(), String>),
    /// An event from the Workshop helper run with this number.
    Workshop(u64, Event),
}

#[derive(Default)]
enum Latest {
    #[default]
    Unknown,
    Checking,
    Known(Release),
    Failed(String),
}

/// The window's state. It's kept while the window is closed, so GitHub isn't asked each time.
#[derive(Default)]
pub struct Offline {
    pub(super) open: bool,
    latest: Latest,
    checked: Option<Instant>,
    installed: Option<Release>,
    missions: Vec<Mission>,
    /// Downloading: bytes so far, and the total.
    downloading: Option<(u64, u64)>,
    resetting: bool,
    /// The mission folder whose save is about to be reset, once confirmed.
    confirm_reset: Option<String>,
    error: Option<String>,
    game_running: bool,
    game_checked: Option<Instant>,
    run: Option<ModRun>,
    /// Steam is the Flatpak, which can't see the browser's data folder unless allowed to.
    flatpak: bool,
}

/// One run of the Workshop helper for a mission's mods.
struct ModRun {
    id: u64,
    helper: workshop::Helper,
    connected: bool,
    /// Bytes done and total for each item downloading.
    items: BTreeMap<u64, (ItemState, u64, u64)>,
    error: Option<String>,
    finished_at: Option<Instant>,
    cancelled: bool,
}

impl ModRun {
    fn running(&self) -> bool {
        self.finished_at.is_none()
    }
}

static NEXT_RUN: AtomicU64 = AtomicU64::new(1);

impl Offline {
    fn busy(&self) -> bool {
        self.downloading.is_some() || self.resetting
    }

    /// Reads which release is unpacked, and its missions (a few small files).
    fn refresh(&mut self) {
        let dir = offline::dir();
        self.installed = offline::installed(&dir);
        self.missions = if self.installed.is_some() {
            offline::installed_missions(&dir)
        } else {
            Vec::new()
        };
        if self
            .confirm_reset
            .as_ref()
            .is_some_and(|f| !self.missions.iter().any(|m| &m.folder == f && m.saved))
        {
            self.confirm_reset = None;
        }
    }

    fn mods_settling(&self) -> bool {
        self.run
            .as_ref()
            .and_then(|r| r.finished_at)
            .is_some_and(|t| t.elapsed() < SETTLE)
    }
}

enum Action {
    CheckAgain,
    Download(Release),
    Play(Mission),
    AskReset(String),
    Reset(Mission),
    KeepSave,
    GetMods(Vec<u64>),
    CancelMods,
    OpenPages(Vec<u64>),
    Close,
}

/// At start: removes the game folder's `Missions` link from an earlier offline game, unless the
/// game is still running (Linux; Windows needs no link).
pub(super) fn remove_link_unless_playing() {
    #[cfg(not(windows))]
    if !launch::game_running() {
        offline::remove_mission_link(&data_dir());
    }
}

impl App {
    pub(super) fn open_offline(&mut self) {
        let window = &mut self.offline;
        if window.open {
            return;
        }
        window.open = true;
        window.error = None;
        window.confirm_reset = None;
        window.game_checked = None;
        window.refresh();
        #[cfg(not(windows))]
        {
            window.flatpak = crate::steam::steam_command().is_ok_and(|c| c[0] == "flatpak");
        }
        let stale = window.checked.is_none_or(|t| t.elapsed() > CHECK_AGAIN)
            || matches!(window.latest, Latest::Failed(_));
        if stale && !matches!(window.latest, Latest::Checking) {
            self.check_offline_release();
        }
        // The Namalsk mods may have come or gone.
        self.reload_local();
    }

    fn check_offline_release(&mut self) {
        // A known release stays shown while it's asked about again.
        if !matches!(self.offline.latest, Latest::Known(_)) {
            self.offline.latest = Latest::Checking;
        }
        self.spawn(|| {
            Msg::Offline(OfflineMsg::Latest(
                offline::latest().map_err(|e| format!("{e:#}")),
            ))
        });
    }

    pub(super) fn offline_msg(&mut self, msg: OfflineMsg) {
        let window = &mut self.offline;
        match msg {
            OfflineMsg::Latest(result) => {
                window.checked = Some(Instant::now());
                match result {
                    Ok(release) => window.latest = Latest::Known(release),
                    Err(e) => {
                        log::warn!("couldn't check for Offline Mode: {e}");
                        // Keep a release found earlier.
                        if !matches!(window.latest, Latest::Known(_)) {
                            window.latest = Latest::Failed(e);
                        }
                    }
                }
            }
            OfflineMsg::Progress(done, total) => {
                if window.downloading.is_some() {
                    window.downloading = Some((done, total));
                }
            }
            OfflineMsg::Downloaded(result) => {
                window.downloading = None;
                if let Err(e) = result {
                    log::warn!("couldn't get Offline Mode: {e}");
                    window.error = Some(e);
                }
                window.refresh();
            }
            OfflineMsg::Reset(title, result) => {
                window.resetting = false;
                window.refresh();
                match result {
                    Ok(()) => self.toast(format!("{title} starts over next time")),
                    Err(e) => self.offline.error = Some(e),
                }
            }
            OfflineMsg::Workshop(run, event) => {
                let Some(r) = window.run.as_mut().filter(|r| r.id == run) else {
                    return;
                };
                let installed = matches!(
                    event,
                    Event::Item {
                        state: ItemState::Installed,
                        ..
                    } | Event::Exited
                );
                match event {
                    Event::Ready => r.connected = true,
                    Event::Item {
                        id,
                        state,
                        done,
                        total,
                        message,
                    } => {
                        if state == ItemState::Failed {
                            r.error = message.or(Some(format!("mod {id} failed to download")));
                        }
                        r.items.insert(id, (state, done, total));
                    }
                    // A cancelled helper's complaints are expected.
                    Event::Error { message } if !r.cancelled => r.error = Some(message),
                    Event::Error { .. } => {}
                    Event::Done => {}
                    Event::Exited => {
                        r.finished_at.get_or_insert_with(Instant::now);
                        if !r.connected && r.error.is_none() && !r.cancelled {
                            r.error =
                                Some("The Workshop helper stopped before reaching Steam".into());
                        }
                    }
                }
                if installed {
                    self.reload_local();
                }
            }
        }
    }

    pub(super) fn offline_window(&mut self, ctx: &egui::Context) {
        if !self.offline.open {
            return;
        }
        if self
            .offline
            .game_checked
            .is_none_or(|t| t.elapsed() > Duration::from_secs(2))
        {
            self.offline.game_running = launch::game_running();
            self.offline.game_checked = Some(Instant::now());
            // Notice mods Steam finishes downloading on its own.
            self.reload_local();
        }
        let install_error = self.install.as_ref().err().cloned();
        let helper = workshop::helper().is_some();
        // Each mission's Workshop mods that are missing, and whether any has an update.
        let needs: Vec<(Vec<u64>, bool)> = self
            .offline
            .missions
            .iter()
            .map(|m| {
                let states: Vec<(u64, ModState)> = m
                    .mods
                    .iter()
                    .map(|&id| (id, self.local.state(id, None)))
                    .collect();
                let missing = states
                    .iter()
                    .filter(|(_, s)| *s == ModState::Missing)
                    .map(|(id, _)| *id)
                    .collect();
                (
                    missing,
                    states.iter().any(|(_, s)| *s == ModState::Outdated),
                )
            })
            .collect();
        let window = &self.offline;
        let blocked: Option<String> = if let Some(e) = &install_error {
            Some(e.clone())
        } else if window.downloading.is_some() {
            Some("Wait for the download to finish".into())
        } else if window.resetting {
            Some("Resetting a save…".into())
        } else if window.run.as_ref().is_some_and(ModRun::running) {
            Some("Wait for the mod downloads to finish".into())
        } else if window.mods_settling() {
            Some("Steam is finishing the downloads…".into())
        } else if window.game_running {
            Some("DayZ is already running".into())
        } else {
            None
        };

        let mut action = None;
        let mut open = true;
        let id = egui::Id::new("offline-window");
        super::windows::placed(egui::Window::new("Play offline"), ctx, id, 440.0)
            .collapsible(false)
            .resizable(false)
            .default_width(440.0)
            .open(&mut open)
            .show(ctx, |ui| {
                ui.label(
                    "Play DayZ on your own, with no server, on your own copy of a map that's \
                     saved between sessions.",
                );
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new("Uses Community Offline Mode, a community project.")
                            .weak()
                            .small(),
                    );
                    ui.hyperlink_to(RichText::new("Project page").small(), offline::PROJECT);
                });
                ui.add_space(6.0);
                release_section(ui, window, &mut action);
                if !window.missions.is_empty() {
                    ui.add_space(6.0);
                    maps_section(ui, window, &needs, blocked.as_deref(), helper, &mut action);
                }
                if window.game_running {
                    ui.label(RichText::new("DayZ is already running; close it first.").color(WARN));
                }
                if let Some(e) = &install_error {
                    ui.label(RichText::new(super::sentence(e)).color(ERROR));
                }
                if let Some(e) = &window.error {
                    ui.label(RichText::new(super::sentence(e)).color(ERROR));
                }
                if !window.missions.is_empty() {
                    ui.add_space(4.0);
                    ui.label(
                        RichText::new(
                            "In the game, Y (Z on German keyboards) opens Offline Mode's menu: \
                             items, teleporting, the free camera, and more.",
                        )
                        .weak()
                        .small(),
                    );
                    if window.flatpak {
                        ui.label(
                            RichText::new(format!(
                                "Flatpak Steam can only start these if it may read the browser's \
                                 folder: flatpak override --user --filesystem={} \
                                 com.valvesoftware.Steam",
                                offline::dir().display()
                            ))
                            .weak()
                            .small(),
                        );
                    }
                }
                ui.add_space(6.0);
                if ui.button("Close").clicked() {
                    action = Some(Action::Close);
                }
            });
        // Not while it's downloading or setting up: closing stops that.
        let busy = self.offline.busy() || self.offline.run.as_ref().is_some_and(ModRun::running);
        if super::windows::escape_closes(ctx, id) && !busy {
            open = false;
        }
        if !open {
            action = Some(Action::Close);
        }
        if let Some(action) = action {
            self.offline_action(action);
        }
        let active = self.offline.busy() || self.offline.run.as_ref().is_some_and(ModRun::running);
        ctx.request_repaint_after(Duration::from_millis(if active { 250 } else { 2000 }));
    }

    fn offline_action(&mut self, action: Action) {
        match action {
            Action::Close => {
                let window = &mut self.offline;
                window.open = false;
                // Stops the helper; Steam keeps downloading anything already subscribed.
                window.run = None;
                window.confirm_reset = None;
            }
            Action::CheckAgain => self.check_offline_release(),
            Action::Download(release) => {
                let window = &mut self.offline;
                if window.busy() {
                    return;
                }
                if window.installed.is_some() && launch::game_running() {
                    window.error = Some(
                        "Close DayZ first: updating replaces the mission files it's using".into(),
                    );
                    return;
                }
                window.error = None;
                window.downloading = Some((0, release.size));
                let (tx, ctx) = (self.tx.clone(), self.ctx.clone());
                std::thread::spawn(move || {
                    let send = |msg: OfflineMsg| {
                        let _ = tx.send(Msg::Offline(msg));
                        ctx.request_repaint();
                    };
                    let mut reported = 0;
                    let result = offline::download(&release, &offline::dir(), |done, total| {
                        // Every quarter megabyte is plenty for a progress bar.
                        if done - reported >= 256 * 1024 || done == total {
                            reported = done;
                            send(OfflineMsg::Progress(done, total));
                        }
                    });
                    send(OfflineMsg::Downloaded(result.map_err(|e| format!("{e:#}"))));
                });
            }
            Action::AskReset(folder) => self.offline.confirm_reset = Some(folder),
            Action::KeepSave => self.offline.confirm_reset = None,
            Action::Reset(mission) => {
                let window = &mut self.offline;
                window.confirm_reset = None;
                if launch::game_running() {
                    window.error = Some("Close DayZ first: it's using the save".into());
                    return;
                }
                window.error = None;
                window.resetting = true;
                self.spawn(move || {
                    Msg::Offline(OfflineMsg::Reset(
                        mission.title.clone(),
                        offline::reset_save(&mission.path).map_err(|e| format!("{e:#}")),
                    ))
                });
            }
            Action::Play(mission) => match self.play_offline(&mission) {
                Ok(()) => {
                    self.offline_action(Action::Close);
                    self.toast(format!(
                        "Starting DayZ offline on {}… (the browser stays open)",
                        mission.title
                    ));
                }
                Err(e) => self.offline.error = Some(format!("{e:#}")),
            },
            Action::OpenPages(ids) => ids.into_iter().for_each(open_workshop),
            Action::GetMods(ids) => {
                let id = NEXT_RUN.fetch_add(1, Ordering::Relaxed);
                let (tx, ctx) = (self.tx.clone(), self.ctx.clone());
                let started = workshop::Helper::start(&ids, move |event| {
                    let _ = tx.send(Msg::Offline(OfflineMsg::Workshop(id, event)));
                    ctx.request_repaint();
                });
                let window = &mut self.offline;
                match started {
                    Ok(helper) => {
                        window.error = None;
                        window.run = Some(ModRun {
                            id,
                            helper,
                            connected: false,
                            items: BTreeMap::new(),
                            error: None,
                            finished_at: None,
                            cancelled: false,
                        });
                    }
                    Err(e) => window.error = Some(format!("{e:#}")),
                }
            }
            Action::CancelMods => {
                // Kept until it settles: Steam may count DayZ as running for a moment yet.
                if let Some(run) = &mut self.offline.run {
                    run.helper.cancel();
                    run.cancelled = true;
                    run.finished_at.get_or_insert_with(Instant::now);
                }
            }
        }
    }

    fn play_offline(&mut self, mission: &Mission) -> anyhow::Result<()> {
        if launch::game_running() {
            anyhow::bail!("DayZ is already running; close it first");
        }
        // A launch with other mods soon after another, before the game shows up, could pull the
        // first one's mod links from under it.
        let other_mods =
            |(at, last): &(Instant, Vec<u64>)| at.elapsed() < RELAUNCH_GAP && *last != mission.mods;
        if self.last_launch.as_ref().is_some_and(other_mods) {
            anyhow::bail!("DayZ is still starting from the last launch; give it a moment");
        }
        let install = self.install.as_ref().map_err(|e| anyhow::anyhow!("{e}"))?;
        let extra = launch::added_args(&self.settings)?;
        let plan = offline::build(install, mission, &extra, &data_dir())?;
        log::info!("launching offline: {}", plan.display());
        if self.settings.overlay && self.overlay != OverlayStatus::Running {
            self.check_overlay(true);
        }
        plan.spawn()?;
        self.last_launch = Some((Instant::now(), mission.mods.clone()));
        // Discord shows it like a server: "Offline Mode", then the map.
        self.discord_playing(&crate::server::Server {
            name: "Offline Mode".into(),
            map: mission.title.clone(),
            ..Default::default()
        });
        Ok(())
    }
}

/// Whether the missions are downloaded, and getting them or their update.
fn release_section(ui: &mut egui::Ui, window: &Offline, action: &mut Option<Action>) {
    if let Some((done, total)) = window.downloading {
        let fraction = if total > 0 {
            done as f32 / total as f32
        } else {
            0.0
        };
        ui.add(
            egui::ProgressBar::new(fraction)
                .desired_width(300.0)
                .text(format!(
                    "Downloading Offline Mode… {} / {}",
                    megabytes(done),
                    megabytes(total)
                )),
        );
        return;
    }
    match (&window.installed, &window.latest) {
        (None, Latest::Unknown | Latest::Checking) => {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Looking up Offline Mode on GitHub…");
            });
        }
        (None, Latest::Known(release)) => {
            ui.label(format!(
                "Offline Mode isn't downloaded yet ({}, {} from GitHub).",
                release.tag,
                megabytes(release.size)
            ));
            if ui.button("⬇ Download").clicked() {
                *action = Some(Action::Download(release.clone()));
            }
        }
        (None, Latest::Failed(e)) => {
            ui.label(RichText::new(format!("Couldn't get Offline Mode: {e}")).color(ERROR));
            if ui.button("Try again").clicked() {
                *action = Some(Action::CheckAgain);
            }
        }
        (Some(installed), Latest::Known(latest)) if offline::changed(Some(installed), latest) => {
            ui.horizontal_wrapped(|ui| {
                ui.label(if latest.tag == installed.tag {
                    format!(
                        "A corrected download of Offline Mode {} is out.",
                        latest.tag
                    )
                } else {
                    format!(
                        "Offline Mode {} is out (you have {}).",
                        latest.tag, installed.tag
                    )
                });
                if ui
                    .button("Update")
                    .on_hover_text(format!(
                        "Downloads it ({}); your saves are kept",
                        megabytes(latest.size)
                    ))
                    .clicked()
                {
                    *action = Some(Action::Download(latest.clone()));
                }
            });
        }
        (Some(installed), latest) => {
            let checked = match latest {
                Latest::Failed(_) => " (couldn't check for a newer one)",
                Latest::Checking => " (checking for a newer one…)",
                _ => "",
            };
            ui.label(RichText::new(format!("Offline Mode {}{checked}", installed.tag)).weak());
        }
    }
}

/// The maps, each with its save, Play, and the mods it needs.
fn maps_section(
    ui: &mut egui::Ui,
    window: &Offline,
    needs: &[(Vec<u64>, bool)],
    blocked: Option<&str>,
    helper: bool,
    action: &mut Option<Action>,
) {
    egui::Grid::new("offline-maps")
        .num_columns(3)
        .spacing([12.0, 6.0])
        .striped(true)
        .show(ui, |ui| {
            for (mission, (missing, outdated)) in window.missions.iter().zip(needs) {
                ui.label(RichText::new(&mission.title).strong());
                ui.label(
                    RichText::new(if mission.saved {
                        "saved game"
                    } else {
                        "new game"
                    })
                    .weak(),
                );
                ui.horizontal(|ui| {
                    if window.confirm_reset.as_deref() == Some(mission.folder.as_str()) {
                        ui.label(RichText::new("Delete this save?").color(WARN));
                        if ui.button("Delete").clicked() {
                            *action = Some(Action::Reset(mission.clone()));
                        }
                        if ui.button("Keep").clicked() {
                            *action = Some(Action::KeepSave);
                        }
                        return;
                    }
                    let why_not = blocked
                        .map(str::to_string)
                        .or((!missing.is_empty()).then(|| "Download its mods first".to_string()));
                    if ui
                        .add_enabled(why_not.is_none(), egui::Button::new("▶ Play"))
                        .on_disabled_hover_text(why_not.unwrap_or_default())
                        .clicked()
                    {
                        *action = Some(Action::Play(mission.clone()));
                    }
                    if mission.saved
                        && ui
                            .add_enabled(!window.busy(), egui::Button::new("Reset save…"))
                            .on_hover_text(
                                "Start this map over: deletes your offline character and \
                                 everything you've done on it",
                            )
                            .clicked()
                    {
                        *action = Some(Action::AskReset(mission.folder.clone()));
                    }
                });
                ui.end_row();
                if !mission.mods.is_empty() && (!missing.is_empty() || *outdated) {
                    ui.label("");
                    mods_row(ui, window, mission, missing, helper, action);
                    ui.end_row();
                }
            }
        });
}

/// What a mission's mods need, and getting them.
fn mods_row(
    ui: &mut egui::Ui,
    window: &Offline,
    mission: &Mission,
    missing: &[u64],
    helper: bool,
    action: &mut Option<Action>,
) {
    let needed = mission.mods.len();
    let label = ui.label(
        RichText::new(if missing.is_empty() {
            "its mods have updates".to_string()
        } else if missing.len() == needed {
            format!("needs {needed} Workshop mods")
        } else {
            format!("needs {} more Workshop mod(s)", missing.len())
        })
        .color(WARN),
    );
    if mission.mods == offline::NAMALSK_MODS {
        label.on_hover_text("Namalsk Island and Namalsk Survival");
    }
    ui.horizontal(|ui| {
        if let Some(run) = window.run.as_ref().filter(|r| r.running()) {
            ui.spinner();
            let (done, total) = run
                .items
                .values()
                .fold((0, 0), |(d, t), (_, done, total)| (d + done, t + total));
            if !run.connected {
                ui.label("Connecting to Steam…");
            } else if total > 0 {
                ui.label(format!("{} / {}", megabytes(done), megabytes(total)));
            } else {
                ui.label("Downloading…");
            }
            if ui.small_button("Cancel").clicked() {
                *action = Some(Action::CancelMods);
            }
            return;
        }
        let need: Vec<u64> = if missing.is_empty() {
            mission.mods.clone()
        } else {
            missing.to_vec()
        };
        if helper
            && ui
                .button("⬇ Download with Steam")
                .on_hover_text(
                    "Subscribes and downloads through your Steam client. Steam shows DayZ as \
                     running until it's done.",
                )
                .clicked()
        {
            *action = Some(Action::GetMods(need.clone()));
        }
        if ui
            .button("Workshop pages")
            .on_hover_text(
                "Subscribe in Steam yourself; this window notices when they're downloaded",
            )
            .clicked()
        {
            *action = Some(Action::OpenPages(need));
        }
    });
    if let Some(e) = window.run.as_ref().and_then(|r| r.error.as_ref()) {
        ui.end_row();
        ui.label("");
        ui.label(RichText::new(super::sentence(e)).color(ERROR));
    }
}

fn megabytes(bytes: u64) -> String {
    format!("{:.0} MB", bytes as f64 / 1_000_000.0)
}
