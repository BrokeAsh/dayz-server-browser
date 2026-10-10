//! Discord Rich Presence in the window: showing the server when DayZ is launched to it, clearing
//! it once the game has closed, and the setting. All of it is hidden while the browser has no
//! Discord application id ([`discord::app_id`]).

use eframe::egui;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use super::App;
use crate::discord::{self, Activity, Presence};
use crate::launch;
use crate::server::Server;

/// How often to look whether the game is still running.
const CHECK_EVERY: Duration = Duration::from_secs(15);
/// A game that hasn't shown up this long after the launch isn't coming (the launch failed, or
/// Steam is busy updating it).
const GIVE_UP: Duration = Duration::from_secs(5 * 60);

#[derive(Default)]
pub struct DiscordStatus {
    /// Started with the first launch.
    presence: Option<Presence>,
    playing: Option<Playing>,
    last_check: Option<Instant>,
}

/// The game launched from the browser.
struct Playing {
    activity: Activity,
    launched: Instant,
    /// The game has been seen running since.
    seen: bool,
}

impl App {
    /// DayZ was launched to this server.
    pub(super) fn discord_playing(&mut self, server: &Server) {
        if discord::app_id().is_none() {
            return;
        }
        let start = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        self.discord.playing = Some(Playing {
            activity: discord::activity(&server.name, &server.map, server.first_person, start),
            launched: Instant::now(),
            seen: false,
        });
        self.discord.last_check = Some(Instant::now());
        self.discord_sync();
    }

    /// Tells Discord what to show: the server being played, if that's on.
    fn discord_sync(&mut self) {
        let Some(id) = discord::app_id() else {
            return;
        };
        let status = &mut self.discord;
        match &status.playing {
            Some(playing) if self.settings.discord_presence => status
                .presence
                .get_or_insert_with(|| Presence::start(id))
                .show(playing.activity.clone()),
            _ => {
                if let Some(presence) = &status.presence {
                    presence.clear();
                }
            }
        }
    }

    /// Every frame: now and then, whether the game launched is still running; clears the
    /// activity once it isn't.
    pub(super) fn discord_now_and_then(&mut self) {
        let Some(playing) = &mut self.discord.playing else {
            return;
        };
        if self
            .discord
            .last_check
            .is_some_and(|t| t.elapsed() < CHECK_EVERY)
        {
            return;
        }
        self.discord.last_check = Some(Instant::now());
        if launch::game_running_lately() {
            playing.seen = true;
        } else if playing.seen || playing.launched.elapsed() > GIVE_UP {
            self.discord.playing = None;
            self.discord_sync();
        }
    }

    /// The settings' Discord section; nothing without an application id.
    pub(super) fn discord_section(&mut self, ui: &mut egui::Ui) {
        if discord::app_id().is_none() {
            return;
        }
        ui.separator();
        ui.heading("Discord");
        if ui
            .checkbox(
                &mut self.settings.discord_presence,
                "Show the server I'm playing on in Discord",
            )
            .on_hover_text(
                "After joining from the browser, your Discord status shows the server's name and \
                 map while DayZ runs. Needs the Discord app running on this computer.",
            )
            .changed()
        {
            self.discord_sync();
        }
    }

    /// When the browser closes: clears the activity.
    pub(super) fn discord_exit(&mut self) {
        if let Some(presence) = self.discord.presence.take() {
            presence.stop();
        }
    }
}
