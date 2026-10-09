//! The join window: checks the server's mods against what's installed, gets missing ones through
//! Steam, asks for the password if there is one, and launches the game.

use eframe::egui::{self, Color32, RichText};
use std::collections::BTreeMap;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use super::{App, Fetch, Msg, OverlayStatus, open_workshop, state_badge};
use crate::launch;
use crate::mods::ModState;
use crate::server::map_title;
use crate::settings::{Recent, data_dir};
use crate::steam_api;
use crate::workshop::{self, Event, ItemState};

pub struct Join {
    pub addr: String,
    password: String,
    run: Option<WorkshopRun>,
    message: Option<(String, bool)>,
    game_running: bool,
    game_checked: Option<Instant>,
}

struct WorkshopRun {
    helper: workshop::Helper,
    connected: bool,
    items: BTreeMap<u64, Progress>,
    error: Option<String>,
    finished: bool,
}

#[derive(Clone)]
struct Progress {
    state: ItemState,
    done: u64,
    total: u64,
    message: Option<String>,
}

impl Join {
    pub fn new(addr: &str) -> Self {
        Self {
            addr: addr.to_string(),
            password: String::new(),
            run: None,
            message: None,
            game_running: false,
            game_checked: None,
        }
    }

    pub fn workshop_event(&mut self, event: Event) {
        let Some(run) = &mut self.run else { return };
        match event {
            Event::Ready => run.connected = true,
            Event::Item {
                id,
                state,
                done,
                total,
                message,
            } => {
                run.items.insert(
                    id,
                    Progress {
                        state,
                        done,
                        total,
                        message,
                    },
                );
            }
            Event::Error { message } => run.error = Some(message),
            Event::Done => {}
            Event::Exited => {
                run.finished = true;
                if !run.connected && run.error.is_none() {
                    run.error = Some("The Workshop helper stopped before reaching Steam".into());
                }
            }
        }
    }

    fn downloading(&self) -> bool {
        self.run.as_ref().is_some_and(|r| !r.finished)
    }
}

enum Action {
    Download(Vec<u64>),
    Subscribe(Vec<u64>),
    OpenPages(Vec<u64>),
    CancelDownload,
    Launch(Vec<u64>),
    Close,
}

impl App {
    pub(super) fn join_window(&mut self, ctx: &egui::Context) {
        let Some(addr) = self.join.as_ref().map(|j| j.addr.clone()) else {
            return;
        };
        let Some(&index) = self.index.get(&addr) else {
            self.join = None;
            return;
        };
        let server = self.servers[index].clone();
        let info = self.live.get(&addr).and_then(|l| l.info.clone());
        if self.local_loaded.elapsed() > Duration::from_secs(2) {
            self.reload_local();
        }
        {
            let join = self.join.as_mut().unwrap();
            if join
                .game_checked
                .is_none_or(|t| t.elapsed() > Duration::from_secs(2))
            {
                join.game_running = launch::game_running();
                join.game_checked = Some(Instant::now());
            }
        }
        let (mods, origin) = self.mods_for(&addr);
        let loading = matches!(self.rules.get(&addr), Some(Fetch::Loading));
        let truncated = matches!(self.rules.get(&addr), Some(Fetch::Done(r)) if r.truncated);
        let states: Vec<_> = mods
            .iter()
            .flatten()
            .map(|m| (m.clone(), self.mod_state(m)))
            .collect();
        let need: Vec<u64> = states
            .iter()
            .filter(|(m, s)| m.id != 0 && matches!(s, ModState::Missing | ModState::Outdated))
            .map(|(m, _)| m.id)
            .collect();
        let missing = states
            .iter()
            .filter(|(_, s)| *s == ModState::Missing)
            .count();
        let outdated = states
            .iter()
            .filter(|(_, s)| *s == ModState::Outdated)
            .count();
        let unknown = states
            .iter()
            .filter(|(_, s)| *s == ModState::NotWorkshop)
            .count();
        let locked = server.password || info.as_ref().is_some_and(|i| i.password);
        let helper = workshop::helper().is_some();
        let has_key = self.key.is_some();
        let install_error = self.install.as_ref().err().cloned();

        let join = self.join.as_mut().unwrap();
        let mut action: Option<Action> = None;
        let mut open = true;
        egui::Window::new("Join server")
            .collapsible(false)
            .resizable(true)
            .default_width(480.0)
            .open(&mut open)
            .show(ctx, |ui| {
                ui.label(RichText::new(&server.name).strong().size(16.0));
                ui.label(
                    RichText::new(format!(
                        "{} · {}/{} players",
                        map_title(&server.map),
                        info.as_ref().map_or(server.players, |i| i.players),
                        info.as_ref().map_or(server.max_players, |i| i.max_players)
                    ))
                    .weak(),
                );
                ui.add_space(8.0);

                if loading && mods.is_none() {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("Asking the server for its mod list…");
                    });
                } else if mods.is_none() {
                    ui.label(
                        RichText::new("The server didn't send its mod list, so it can't be joined from here.")
                            .color(Color32::from_rgb(230, 120, 90)),
                    );
                } else if states.is_empty() {
                    ui.label("No mods needed.");
                } else {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(format!("{} mods", states.len())).strong());
                        if loading {
                            ui.spinner();
                        } else {
                            ui.label(RichText::new(origin).weak().small());
                        }
                    });
                    egui::ScrollArea::vertical().max_height(260.0).id_salt("join-mods").show(ui, |ui| {
                        egui::Grid::new("join-mod-grid").num_columns(3).spacing([8.0, 3.0]).striped(true).show(ui, |ui| {
                            for (m, state) in &states {
                                let (icon, color, tip) = state_badge(*state);
                                ui.label(RichText::new(icon).color(color)).on_hover_text(tip);
                                ui.label(&m.name);
                                let progress = join.run.as_ref().and_then(|r| r.items.get(&m.id));
                                match (progress, state) {
                                    (Some(p), _) if p.state != ItemState::Installed || *state != ModState::Installed => {
                                        progress_cell(ui, p);
                                    }
                                    (_, ModState::Missing | ModState::Outdated) => {
                                        if ui.small_button("Workshop page").clicked() {
                                            action = Some(Action::OpenPages(vec![m.id]));
                                        }
                                    }
                                    _ => {
                                        ui.label("");
                                    }
                                }
                                ui.end_row();
                            }
                        });
                    });
                }
                if truncated {
                    ui.label(RichText::new("The server's list was cut short; it may need more mods than shown.").weak().small());
                }
                if unknown > 0 {
                    ui.label(
                        RichText::new(format!("{unknown} mod(s) aren't on the Steam Workshop; the server has to provide them."))
                            .weak()
                            .small(),
                    );
                }

                // Getting what's missing.
                if !need.is_empty() {
                    ui.add_space(8.0);
                    let mut parts = Vec::new();
                    if missing > 0 {
                        parts.push(format!("{missing} to download"));
                    }
                    if outdated > 0 {
                        parts.push(format!("{outdated} to update"));
                    }
                    ui.label(RichText::new(parts.join(", ")).strong());
                    ui.horizontal_wrapped(|ui| {
                        if helper {
                            let busy = join.downloading();
                            if ui
                                .add_enabled(!busy, egui::Button::new("⬇ Download with Steam"))
                                .on_hover_text(
                                    "Subscribes and downloads through your Steam client. Steam shows \
                                     DayZ as running until it's done.",
                                )
                                .clicked()
                            {
                                action = Some(Action::Download(need.clone()));
                            }
                            if busy && ui.button("Cancel").clicked() {
                                action = Some(Action::CancelDownload);
                            }
                        } else if has_key && ui.button("Subscribe with my Steam key").clicked() {
                            action = Some(Action::Subscribe(need.clone()));
                        }
                        if ui
                            .button("Open Workshop pages")
                            .on_hover_text("Subscribe in Steam yourself; this window notices when they're downloaded")
                            .clicked()
                        {
                            action = Some(Action::OpenPages(need.clone()));
                        }
                    });
                }
                if let Some(run) = &join.run {
                    if let Some(e) = &run.error {
                        ui.label(RichText::new(e).color(Color32::from_rgb(230, 120, 90)));
                    } else if !run.finished && !run.connected {
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.label("Connecting to Steam…");
                        });
                    } else if !run.finished {
                        ui.label(RichText::new("Downloading through Steam (it shows DayZ as running until done)…").weak());
                    }
                }

                ui.add_space(8.0);
                if locked {
                    ui.horizontal(|ui| {
                        ui.label("Password");
                        ui.add(egui::TextEdit::singleline(&mut join.password).password(true).desired_width(200.0));
                    });
                }
                let (players, max) = info
                    .as_ref()
                    .map_or((server.players, server.max_players), |i| (i.players, i.max_players));
                if max > 0 && players >= max {
                    ui.label(
                        RichText::new("The server is full: DayZ may queue you or turn you away to the main menu.")
                            .color(Color32::from_rgb(230, 170, 90)),
                    );
                }
                if join.game_running {
                    ui.label(RichText::new("DayZ is already running; close it first.").color(Color32::from_rgb(230, 170, 90)));
                }
                if let Some(e) = &install_error {
                    ui.label(RichText::new(e).color(Color32::from_rgb(230, 120, 90)));
                }
                if outdated > 0 && missing == 0 && !join.downloading() {
                    ui.label(RichText::new("Some mods have updates; joining may fail until they're updated.").weak().small());
                }
                if let Some((message, error)) = &join.message {
                    let color = if *error { Color32::from_rgb(230, 120, 90) } else { ui.visuals().text_color() };
                    ui.label(RichText::new(message).color(color));
                }
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    let ready = mods.is_some()
                        && missing == 0
                        && !join.downloading()
                        && !join.game_running
                        && install_error.is_none();
                    let launch = ui.add_enabled(ready, egui::Button::new(RichText::new("  Launch DayZ  ").strong()));
                    if launch.clicked() {
                        let ids = mods.iter().flatten().filter(|m| m.id != 0).map(|m| m.id).collect();
                        action = Some(Action::Launch(ids));
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
            self.join_action(action, &server, locked);
        }
        if self.join.as_ref().is_some_and(Join::downloading) {
            ctx.request_repaint_after(Duration::from_millis(250));
        } else if self.join.is_some() {
            // Notice mods Steam finishes downloading on its own.
            ctx.request_repaint_after(Duration::from_secs(2));
        }
    }

    fn join_action(&mut self, action: Action, server: &crate::server::Server, locked: bool) {
        match action {
            Action::Close => {
                // Closing stops the helper; Steam keeps downloading anything already subscribed.
                if let Some(mut join) = self.join.take()
                    && let Some(mut run) = join.run.take()
                    && !run.finished
                {
                    run.helper.cancel();
                }
            }
            Action::OpenPages(ids) => {
                for id in ids.iter().take(15) {
                    open_workshop(*id);
                }
                if ids.len() > 15 {
                    self.toast(format!("Opened the first 15 of {} pages", ids.len()));
                }
            }
            Action::Download(ids) => {
                let tx = self.tx.clone();
                let ctx = self.ctx.clone();
                let started = workshop::Helper::start(&ids, move |event| {
                    let _ = tx.send(Msg::Workshop(event));
                    ctx.request_repaint();
                });
                let join = self.join.as_mut().unwrap();
                match started {
                    Ok(helper) => {
                        join.message = None;
                        join.run = Some(WorkshopRun {
                            helper,
                            connected: false,
                            items: BTreeMap::new(),
                            error: None,
                            finished: false,
                        });
                    }
                    Err(e) => join.message = Some((format!("{e:#}"), true)),
                }
            }
            Action::CancelDownload => {
                if let Some(run) = self.join.as_mut().and_then(|j| j.run.as_mut()) {
                    run.helper.cancel();
                    run.finished = true;
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
            Action::Launch(ids) => match self.launch(server, &ids, locked) {
                Ok(()) => {
                    self.join = None;
                    self.toast("Starting DayZ… (the browser stays open)");
                }
                Err(e) => {
                    if let Some(join) = self.join.as_mut() {
                        join.message = Some((format!("{e:#}"), true));
                    }
                }
            },
        }
    }

    fn launch(
        &mut self,
        server: &crate::server::Server,
        ids: &[u64],
        locked: bool,
    ) -> anyhow::Result<()> {
        let install = self.install.as_ref().map_err(|e| anyhow::anyhow!("{e}"))?;
        let extra = launch::parse_extra(&self.settings.extra_args)?;
        let password = self
            .join
            .as_ref()
            .map(|j| j.password.clone())
            .filter(|_| locked);
        let plan = launch::build(
            install,
            server,
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
        let joined = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        self.state.add_recent(Recent {
            addr: server.addr(),
            name: server.name.clone(),
            map: server.map.clone(),
            joined,
        });
        self.state_changed();
        Ok(())
    }
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
        ItemState::Installed => {
            ui.label(RichText::new("done").color(Color32::from_rgb(110, 200, 120)));
        }
        ItemState::Failed => {
            let label = ui.label(RichText::new("failed").color(Color32::from_rgb(230, 120, 90)));
            if let Some(m) = &p.message {
                label.on_hover_text(m);
            }
        }
    }
}

fn megabytes(bytes: u64) -> String {
    format!("{:.0} MB", bytes as f64 / 1_000_000.0)
}
