//! The settings window: which server list to use, the Steam Web API key, extra launch arguments,
//! the DayZ folder, the map overlay (on or off, and its hotkey), and servers added by address.

use eframe::egui::{self, RichText};
use std::path::PathBuf;

use super::{App, ERROR, Msg, OverlayStatus};
use crate::launch;
use crate::mods::LocalMods;
use crate::overlay;
use crate::secret::{self, Storage};
use crate::settings::{Settings, Source};
use crate::steam;
use crate::steam_api;

pub struct SettingsWindow {
    key_input: String,
    key_busy: bool,
    key_error: Option<String>,
    /// Edited here and applied when the field loses focus or the window closes, rather than
    /// saving the settings on every keystroke.
    extra_args: String,
    game_dir: String,
    game_message: Option<String>,
    overlay_program: String,
    hotkey: String,
    capturing: bool,
    hotkey_message: Option<(String, bool)>,
}

fn shown(path: Option<&PathBuf>) -> String {
    path.map(|p| p.display().to_string()).unwrap_or_default()
}

impl SettingsWindow {
    pub fn new(settings: &Settings) -> Self {
        Self {
            key_input: String::new(),
            key_busy: false,
            key_error: None,
            extra_args: settings.extra_args.clone(),
            game_dir: shown(settings.game_dir.as_ref()),
            game_message: None,
            overlay_program: shown(settings.overlay_program.as_ref()),
            hotkey: overlay::hotkey(),
            capturing: false,
            hotkey_message: None,
        }
    }

    /// The result of testing and saving a Steam key. Once saved, the key section says where.
    pub fn key_result(&mut self, result: &Result<Storage, String>) {
        self.key_busy = false;
        match result {
            Ok(_) => {
                self.key_input.clear();
                self.key_error = None;
            }
            Err(e) => self.key_error = Some(e.clone()),
        }
    }
}

impl App {
    pub(super) fn settings_window(&mut self, ctx: &egui::Context) {
        let Some(mut window) = self.settings_window.take() else {
            return;
        };
        let before = self.settings.clone();
        let mut open = true;
        egui::Window::new("Settings")
            .collapsible(false)
            .resizable(false)
            .default_width(480.0)
            .open(&mut open)
            .show(ctx, |ui| {
                self.list_section(ui, &mut window);
                ui.separator();
                self.launch_section(ui, &mut window);
                ui.separator();
                self.overlay_section(ui, &mut window);
                if !self.state.added.is_empty() {
                    ui.separator();
                    self.added_section(ui);
                }
            });
        if !open {
            self.settings.extra_args = window.extra_args.clone();
        }
        if self.settings != before
            && let Err(e) = self.settings.save()
        {
            self.toast(format!("Couldn't save the settings: {e:#}"));
        }
        if open {
            self.settings_window = Some(window);
        }
    }

    fn list_section(&mut self, ui: &mut egui::Ui, window: &mut SettingsWindow) {
        ui.heading("Server list");
        let mut changed = false;
        ui.horizontal(|ui| {
            changed |= ui
                .radio_value(&mut self.settings.source, Source::Dzsa, "DZSA (default)")
                .changed();
            changed |= ui
                .radio_value(
                    &mut self.settings.source,
                    Source::Steam,
                    "Steam, with my key",
                )
                .changed();
        });
        ui.label(
            RichText::new(
                "DZSA's list needs no setup and includes each server's mods. Steam's list comes \
                 straight from Valve and needs your own Steam Web API key.",
            )
            .weak()
            .small(),
        );
        // The key only matters for Steam's list.
        if self.settings.source == Source::Steam {
            ui.add_space(6.0);
            changed |= self.key_section(ui, window);
        }
        if changed {
            self.source_changed();
        }
    }

    /// The Steam Web API key: where it's kept, or a field to test and save one. Returns whether
    /// removing it switched the list back to DZSA's.
    fn key_section(&mut self, ui: &mut egui::Ui, window: &mut SettingsWindow) -> bool {
        ui.label(RichText::new("Steam Web API key").strong());
        if let Some((_, storage)) = &self.key {
            let mut remove = false;
            ui.horizontal(|ui| {
                ui.label(format!("Saved in {}", storage.describe()));
                remove = ui.button("Remove").clicked();
            });
            if remove {
                secret::remove();
                self.key = None;
                self.toast("Steam key removed");
                self.settings.source = Source::Dzsa;
                return true;
            }
        } else {
            ui.horizontal(|ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut window.key_input)
                        .password(true)
                        .hint_text("32 letters and digits")
                        .desired_width(260.0),
                );
                let key = window.key_input.trim().to_string();
                let enabled = !window.key_busy && !key.is_empty();
                if ui
                    .add_enabled(enabled, egui::Button::new("Test and save"))
                    .clicked()
                {
                    window.key_busy = true;
                    window.key_error = None;
                    self.spawn(move || {
                        Msg::KeyTest(
                            steam_api::test_key(&key)
                                .map_err(|e| format!("{e:#}"))
                                .and_then(|_| secret::save(&key).map_err(|e| format!("{e:#}"))),
                        )
                    });
                }
                if window.key_busy {
                    ui.spinner();
                }
            });
            if ui.link("Get a key from Steam").clicked() {
                let _ = open::that_detached("https://steamcommunity.com/dev/apikey");
            }
        }
        if let Some(e) = &window.key_error {
            ui.label(RichText::new(e).color(ERROR));
        }
        ui.label(
            RichText::new(
                "Kept in your system keyring, never in the settings file. Don't share it.",
            )
            .weak()
            .small(),
        );
        false
    }

    /// Switching lists: don't show one list under the other's name while the new one loads.
    fn source_changed(&mut self) {
        self.set_servers(Vec::new());
        self.list.fetched = None;
        self.list.last_request = None;
        self.load_list(false);
    }

    fn launch_section(&mut self, ui: &mut egui::Ui, window: &mut SettingsWindow) {
        ui.heading("Launching");
        ui.label("Extra launch arguments");
        let edit = ui.add(
            egui::TextEdit::singleline(&mut window.extra_args)
                .hint_text("-nosplash -skipIntro -world=empty")
                .desired_width(f32::INFINITY),
        );
        if edit.lost_focus() {
            self.settings.extra_args = window.extra_args.clone();
        }
        match launch::parse_extra(&window.extra_args) {
            Ok(args) => {
                let conflicts = launch::managed_conflicts(&args);
                if !conflicts.is_empty() {
                    ui.label(
                        RichText::new(format!(
                            "The browser sets these itself; remove them: {}",
                            conflicts.join(" ")
                        ))
                        .color(ERROR),
                    );
                }
            }
            Err(e) => {
                ui.label(RichText::new(format!("{e:#}")).color(ERROR));
            }
        }

        ui.add_space(6.0);
        ui.label("DayZ folder");
        match &self.install {
            Ok(install) => ui.label(
                RichText::new(match &self.game_version {
                    Some(v) => format!(
                        "DayZ {v} in {} (from {})",
                        install.game.display(),
                        install.how
                    ),
                    None => format!("{} (from {})", install.game.display(), install.how),
                })
                .weak(),
            ),
            Err(e) => ui.label(RichText::new(e).color(ERROR)),
        };
        let mut use_dir: Option<Option<PathBuf>> = None;
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut window.game_dir)
                    .hint_text("found through Steam")
                    .desired_width(300.0),
            );
            if ui.button("Use").clicked() && !window.game_dir.trim().is_empty() {
                use_dir = Some(Some(PathBuf::from(window.game_dir.trim())));
            }
            if self.settings.game_dir.is_some() && ui.button("Find automatically").clicked() {
                window.game_dir.clear();
                use_dir = Some(None);
            }
        });
        if let Some(message) = &window.game_message {
            ui.label(RichText::new(message).color(ERROR));
        }
        if let Some(dir) = use_dir {
            match steam::find(dir.as_deref()) {
                Ok(install) => {
                    self.local = LocalMods::load(&install);
                    self.install = Ok(install);
                    self.settings.game_dir = dir;
                    window.game_message = None;
                    self.read_game_version();
                }
                Err(e) => window.game_message = Some(format!("{e:#}")),
            }
        }
    }

    fn overlay_section(&mut self, ui: &mut egui::Ui, window: &mut SettingsWindow) {
        ui.heading("Map overlay");
        if ui
            .checkbox(
                &mut self.settings.overlay,
                "Run the DayZ map overlay with the browser",
            )
            .changed()
        {
            self.check_overlay(true);
        }
        match overlay::program(&self.settings) {
            Some(path) => {
                ui.label(RichText::new(path.display().to_string()).weak().small());
            }
            None => {
                ui.label(
                    RichText::new("The overlay (dayz-map) isn't installed or wasn't found.")
                        .color(ERROR),
                );
            }
        }
        ui.horizontal(|ui| {
            ui.label("Program");
            let edit = ui.add(
                egui::TextEdit::singleline(&mut window.overlay_program)
                    .hint_text("found automatically")
                    .desired_width(240.0),
            );
            let enter = edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if ui.button("Use").clicked() || enter {
                let text = window.overlay_program.trim();
                let program = (!text.is_empty()).then(|| PathBuf::from(text));
                if program != self.settings.overlay_program {
                    self.settings.overlay_program = program;
                    self.check_overlay(true);
                }
            }
        });
        let mut new_hotkey = None;
        ui.horizontal(|ui| {
            ui.label("Hotkey in game");
            if window.capturing {
                ui.label(RichText::new("Press a key… (Esc cancels)").strong());
                let pressed = ui.input(|i| {
                    i.events.iter().find_map(|e| match e {
                        egui::Event::Key {
                            key, pressed: true, ..
                        } => Some(*key),
                        _ => None,
                    })
                });
                if let Some(key) = pressed {
                    window.capturing = false;
                    if key == egui::Key::Escape {
                        window.hotkey_message = None;
                    } else if let Some(name) = overlay::key_name(key) {
                        new_hotkey = Some(name);
                    } else {
                        window.hotkey_message = Some((
                            "That key can't be used; pick a letter, digit, F1-F24, Tab, or `"
                                .into(),
                            true,
                        ));
                    }
                }
            } else {
                ui.label(
                    RichText::new(overlay::key_label(&window.hotkey))
                        .strong()
                        .monospace(),
                );
                if ui.button("Change").clicked() {
                    window.capturing = true;
                    window.hotkey_message = None;
                }
            }
        });
        if let Some((message, error)) = &window.hotkey_message {
            ui.label(if *error {
                RichText::new(message).color(ERROR)
            } else {
                RichText::new(message).weak()
            });
        }
        if let Some(name) = new_hotkey {
            self.set_hotkey(window, name);
        }
    }

    /// Saves a new overlay hotkey, and restarts the overlay if it's running (however it was
    /// started) so it takes effect.
    fn set_hotkey(&mut self, window: &mut SettingsWindow, name: String) {
        if let Err(e) = overlay::set_hotkey(&name) {
            window.hotkey_message = Some((format!("{e:#}"), true));
            return;
        }
        window.hotkey = name;
        window.hotkey_message = Some(("Saved".into(), false));
        let Some(program) = overlay::program(&self.settings) else {
            return;
        };
        self.spawn(move || {
            if !overlay::is_running(&program) {
                return Msg::Toast("The overlay uses the new hotkey when it next starts".into());
            }
            match overlay::restart(&program) {
                Ok(()) => {
                    std::thread::sleep(std::time::Duration::from_secs(1));
                    Msg::Overlay(if overlay::is_running(&program) {
                        OverlayStatus::Running
                    } else {
                        OverlayStatus::Failed("didn't come back after the restart".into())
                    })
                }
                Err(e) => Msg::Overlay(OverlayStatus::Failed(format!("{e:#}"))),
            }
        });
    }

    /// Servers added by address, including ones that didn't answer this time (so they can still
    /// be removed).
    fn added_section(&mut self, ui: &mut egui::Ui) {
        ui.heading("Servers added by address");
        let mut forget = None;
        for addr in &self.state.added {
            ui.horizontal(|ui| {
                match self.added.iter().find(|s| s.addr() == *addr) {
                    Some(s) => ui.label(&s.name).on_hover_text(addr),
                    None => ui.label(RichText::new(format!("{addr} (didn't answer)")).weak()),
                };
                if ui.small_button("Remove").clicked() {
                    forget = Some(addr.clone());
                }
            });
        }
        if let Some(addr) = forget {
            self.forget_added(&addr);
        }
    }
}
