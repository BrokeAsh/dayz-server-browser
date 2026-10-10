//! Updating from the browser: the check for a new version, the Update button in the top bar, and
//! the Updates section of the settings.

use eframe::egui::{self, RichText};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use super::settings::SettingsWindow;
use super::{App, ERROR, GOOD, Msg};
use crate::update::{self, Release};
use crate::{launch, overlay};

/// While the browser stays open, look for a new version again after this long.
const CHECK_AGAIN: Duration = Duration::from_secs(12 * 60 * 60);

#[derive(Default)]
pub enum UpdateStatus {
    /// Not checked (or checking is off).
    #[default]
    Idle,
    Checking,
    UpToDate,
    CheckFailed(String),
    Available(Release),
    /// Downloading: bytes so far, and the total if known.
    Downloading(Release, u64, Option<u64>),
    Installing(Release),
    Failed(Release, String),
}

pub enum UpdateMsg {
    Checked(Result<Option<Release>, String>),
    Progress(u64, Option<u64>),
    Installing,
    /// The new browser program, once its files are in place.
    Installed(Result<PathBuf, String>),
}

impl UpdateStatus {
    fn busy(&self) -> bool {
        matches!(
            self,
            Self::Checking | Self::Downloading(..) | Self::Installing(_)
        )
    }
}

impl App {
    /// At start and every so often after: looks for a new version, if that's on.
    pub(super) fn check_for_update_now_and_then(&mut self) {
        if self.settings.check_updates
            && !self.update.busy()
            && self
                .last_update_check
                .is_none_or(|t| t.elapsed() > CHECK_AGAIN)
        {
            self.check_for_update();
        }
    }

    fn check_for_update(&mut self) {
        // A found update stays found while it's checked again.
        if !matches!(self.update, UpdateStatus::Available(_)) {
            self.update = UpdateStatus::Checking;
        }
        self.last_update_check = Some(Instant::now());
        self.spawn(|| {
            Msg::Update(UpdateMsg::Checked(
                update::check().map_err(|e| format!("{e:#}")),
            ))
        });
    }

    pub(super) fn update_msg(&mut self, msg: UpdateMsg) {
        let release = match &self.update {
            UpdateStatus::Available(r)
            | UpdateStatus::Downloading(r, ..)
            | UpdateStatus::Installing(r)
            | UpdateStatus::Failed(r, _) => Some(r.clone()),
            _ => None,
        };
        match msg {
            UpdateMsg::Checked(result) => {
                if matches!(
                    self.update,
                    UpdateStatus::Downloading(..) | UpdateStatus::Installing(_)
                ) {
                    return;
                }
                self.update = match result {
                    Ok(Some(release)) => {
                        log::info!("version {} is available", release.version);
                        UpdateStatus::Available(release)
                    }
                    Ok(None) => UpdateStatus::UpToDate,
                    Err(e) => {
                        log::warn!("couldn't check for a new version: {e}");
                        // Keep offering an update found earlier.
                        match release {
                            Some(r) => UpdateStatus::Available(r),
                            None => UpdateStatus::CheckFailed(e),
                        }
                    }
                };
            }
            UpdateMsg::Progress(done, total) => {
                if let UpdateStatus::Downloading(_, d, t) = &mut self.update {
                    (*d, *t) = (done, total);
                }
            }
            UpdateMsg::Installing => {
                if let Some(r) = release {
                    self.update = UpdateStatus::Installing(r);
                }
            }
            UpdateMsg::Installed(Ok(program)) => self.restart_into(&program),
            UpdateMsg::Installed(Err(e)) => {
                log::warn!("the update failed: {e}");
                if let Some(r) = release {
                    self.update = UpdateStatus::Failed(r, e);
                }
                // It was stopped for the update.
                self.check_overlay(true);
            }
        }
    }

    /// Why updating has to wait, if it does.
    fn update_blocker(&self) -> Option<&'static str> {
        if launch::game_running() {
            Some("Close DayZ first: updating restarts the map overlay")
        } else if self.join.as_ref().is_some_and(|j| j.downloading()) {
            Some("Wait for the mod downloads to finish")
        } else {
            None
        }
    }

    fn start_update(&mut self, release: Release) {
        log::info!("updating to {}", release.version);
        self.update = UpdateStatus::Downloading(release.clone(), 0, None);
        let tx = self.tx.clone();
        let ctx = self.ctx.clone();
        std::thread::spawn(move || {
            let send = |msg: UpdateMsg| {
                let _ = tx.send(Msg::Update(msg));
                ctx.request_repaint();
            };
            let mut reported = 0;
            let result = update::update(
                &release,
                |done, total| {
                    // Every half megabyte is plenty for a progress label.
                    if done - reported >= 512 * 1024 || Some(done) == total {
                        reported = done;
                        send(UpdateMsg::Progress(done, total));
                    }
                },
                || {
                    send(UpdateMsg::Installing);
                    // The new browser starts the new overlay.
                    if let Some(program) = overlay::program()
                        && let Err(e) = overlay::quit(&program)
                    {
                        log::warn!("couldn't stop the overlay for the update: {e:#}");
                    }
                },
            );
            send(UpdateMsg::Installed(result.map_err(|e| format!("{e:#}"))));
        });
    }

    /// Starts the new version and closes this one.
    fn restart_into(&mut self, program: &std::path::Path) {
        if self.state_changed.take().is_some()
            && let Err(e) = self.state.save()
        {
            log::warn!("couldn't save the browser state: {e:#}");
        }
        match update::start(program) {
            Ok(()) => {
                log::info!("updated; starting the new version");
                self.ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            Err(e) => {
                let release = match &self.update {
                    UpdateStatus::Installing(r) => r.clone(),
                    _ => return,
                };
                self.update = UpdateStatus::Failed(
                    release,
                    format!(
                        "updated, but couldn't start the new version ({e:#}); start it again yourself"
                    ),
                );
            }
        }
    }

    /// The top bar's part: an Update button when there's a new version, then its progress.
    pub(super) fn update_button(&mut self, ui: &mut egui::Ui) {
        let mut start = None;
        match &self.update {
            UpdateStatus::Available(release) | UpdateStatus::Failed(release, _) => {
                let failed = match &self.update {
                    UpdateStatus::Failed(_, e) => Some(e.clone()),
                    _ => None,
                };
                if ui.link("What's new").clicked() {
                    let _ = open::that(&release.page);
                }
                let blocker = self.update_blocker();
                let label = if failed.is_some() {
                    "⬆ Try the update again".to_string()
                } else {
                    format!("⬆ Update to {}", release.version)
                };
                let button = ui
                    .add_enabled(
                        blocker.is_none(),
                        egui::Button::new(RichText::new(label).strong().color(GOOD)),
                    )
                    .on_hover_text(format!(
                        "Downloads version {} and restarts the browser and the map overlay",
                        release.version
                    ))
                    .on_disabled_hover_text(blocker.unwrap_or_default());
                if button.clicked() {
                    start = Some(release.clone());
                }
                if let Some(e) = failed {
                    ui.label(RichText::new("Update failed").color(ERROR))
                        .on_hover_text(e);
                }
            }
            UpdateStatus::Downloading(_, done, total) => {
                let mb = |b: u64| b as f64 / 1_048_576.0;
                ui.label(match total {
                    Some(total) => format!("Updating: {:.0} of {:.0} MB", mb(*done), mb(*total)),
                    None => format!("Updating: {:.0} MB", mb(*done)),
                });
                ui.spinner();
            }
            UpdateStatus::Installing(_) => {
                ui.label("Installing the update…");
                ui.spinner();
            }
            _ => {}
        }
        if let Some(release) = start {
            self.start_update(release);
        }
    }

    /// The settings' Updates section.
    pub(super) fn updates_section(&mut self, ui: &mut egui::Ui, window: &SettingsWindow) {
        ui.heading("Updates");
        ui.checkbox(
            &mut self.settings.check_updates,
            "Check for a new version when the browser starts",
        )
        .on_hover_text(
            "Asks GitHub, where the browser's releases are, and again twice a day while it's open",
        );
        let mut check = false;
        ui.horizontal(|ui| {
            ui.label(match &window.overlay_version {
                Some(overlay) => format!("Browser {}, map overlay {overlay}.", update::VERSION),
                None => format!("Browser {}.", update::VERSION),
            })
            .on_hover_text("Updates bring both: the overlay comes with the browser");
            check = ui
                .add_enabled(!self.update.busy(), egui::Button::new("Check now"))
                .clicked();
            match &self.update {
                UpdateStatus::Checking => {
                    ui.spinner();
                }
                UpdateStatus::UpToDate => {
                    ui.label(RichText::new("✔ Up to date").color(GOOD));
                }
                UpdateStatus::Available(r) => {
                    ui.label(
                        RichText::new(format!("{} is out: use Update at the top.", r.version))
                            .color(GOOD),
                    );
                }
                UpdateStatus::CheckFailed(e) => {
                    ui.label(RichText::new("Couldn't check").color(ERROR))
                        .on_hover_text(e);
                }
                _ => {}
            }
        });
        if check {
            self.check_for_update();
        }
    }
}
