//! The settings window: which server list to use, the Steam Web API key, extra launch arguments,
//! the DayZ folder, and the map overlay (on or off, and its hotkey).

use eframe::egui::{self, Color32, RichText};
use std::path::PathBuf;

use super::{App, Msg, OverlayStatus};
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
    key_message: Option<(String, bool)>,
    game_dir: String,
    game_message: Option<String>,
    overlay_program: String,
    hotkey: String,
    capturing: bool,
    hotkey_message: Option<(String, bool)>,
}

impl SettingsWindow {
    pub fn new(settings: &Settings) -> Self {
        Self {
            key_input: String::new(),
            key_busy: false,
            key_message: None,
            game_dir: settings
                .game_dir
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_default(),
            game_message: None,
            overlay_program: settings
                .overlay_program
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_default(),
            hotkey: overlay::hotkey(),
            capturing: false,
            hotkey_message: None,
        }
    }

    pub fn key_result(&mut self, result: &Result<Storage, String>) {
        self.key_busy = false;
        match result {
            Ok(storage) => {
                self.key_input.clear();
                self.key_message = Some((format!("Key saved in {}", storage.describe()), false));
            }
            Err(e) => self.key_message = Some((e.clone(), true)),
        }
    }
}

const RED: Color32 = Color32::from_rgb(230, 120, 90);

impl App {
    pub(super) fn settings_window(&mut self, ctx: &egui::Context) {
        let Some(mut window) = self.settings_window.take() else {
            return;
        };
        let mut open = true;
        let before = self.settings.clone();
        let mut source_changed = false;
        let mut test_key: Option<String> = None;
        let mut remove_key = false;
        let mut use_game_dir: Option<Option<PathBuf>> = None;
        let mut overlay_toggled = false;
        let mut new_hotkey: Option<String> = None;

        egui::Window::new("Settings")
            .collapsible(false)
            .resizable(false)
            .default_width(480.0)
            .open(&mut open)
            .show(ctx, |ui| {
                ui.heading("Server list");
                ui.horizontal(|ui| {
                    source_changed |= ui
                        .radio_value(&mut self.settings.source, Source::Dzsa, "DZSA (default)")
                        .changed();
                    source_changed |= ui
                        .radio_value(&mut self.settings.source, Source::Steam, "Steam, with my key")
                        .changed();
                });
                ui.label(
                    RichText::new(
                        "DZSA's list needs no setup and includes each server's mods. Steam's list \
                         comes straight from Valve and needs your own Steam Web API key.",
                    )
                    .weak()
                    .small(),
                );

                ui.add_space(6.0);
                ui.label(RichText::new("Steam Web API key").strong());
                match &self.key {
                    Some((_, storage)) => {
                        ui.horizontal(|ui| {
                            ui.label(format!("Saved in {}", storage.describe()));
                            if ui.button("Remove").clicked() {
                                remove_key = true;
                            }
                        });
                    }
                    None => {
                        ui.horizontal(|ui| {
                            ui.add(
                                egui::TextEdit::singleline(&mut window.key_input)
                                    .password(true)
                                    .hint_text("32 letters and digits")
                                    .desired_width(260.0),
                            );
                            let enabled = !window.key_busy && !window.key_input.trim().is_empty();
                            if ui.add_enabled(enabled, egui::Button::new("Test and save")).clicked() {
                                test_key = Some(window.key_input.trim().to_string());
                            }
                            if window.key_busy {
                                ui.spinner();
                            }
                        });
                        if ui.link("Get a key from Steam").clicked() {
                            let _ = open::that_detached("https://steamcommunity.com/dev/apikey");
                        }
                    }
                }
                if let Some((message, error)) = &window.key_message {
                    ui.label(if *error { RichText::new(message).color(RED) } else { RichText::new(message) });
                }
                ui.label(
                    RichText::new("Kept in your system keyring, never in the settings file. Don't share it.")
                        .weak()
                        .small(),
                );

                ui.separator();
                ui.heading("Launching");
                ui.label("Extra launch arguments");
                ui.add(
                    egui::TextEdit::singleline(&mut self.settings.extra_args)
                        .hint_text("-nosplash -skipIntro -world=empty")
                        .desired_width(f32::INFINITY),
                );
                match launch::parse_extra(&self.settings.extra_args) {
                    Ok(args) => {
                        let conflicts = launch::managed_conflicts(&args);
                        if !conflicts.is_empty() {
                            ui.label(
                                RichText::new(format!(
                                    "The browser sets these itself; remove them: {}",
                                    conflicts.join(" ")
                                ))
                                .color(RED),
                            );
                        }
                    }
                    Err(e) => {
                        ui.label(RichText::new(format!("{e:#}")).color(RED));
                    }
                }

                ui.add_space(6.0);
                ui.label("DayZ folder");
                match &self.install {
                    Ok(install) => ui.label(
                        RichText::new(format!("{} (from {})", install.game.display(), install.how)).weak(),
                    ),
                    Err(e) => ui.label(RichText::new(e).color(RED)),
                };
                ui.horizontal(|ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut window.game_dir)
                            .hint_text("found through Steam")
                            .desired_width(300.0),
                    );
                    if ui.button("Use").clicked() && !window.game_dir.trim().is_empty() {
                        use_game_dir = Some(Some(PathBuf::from(window.game_dir.trim())));
                    }
                    if self.settings.game_dir.is_some() && ui.button("Find automatically").clicked() {
                        window.game_dir.clear();
                        use_game_dir = Some(None);
                    }
                });
                if let Some(message) = &window.game_message {
                    ui.label(RichText::new(message).color(RED));
                }

                ui.separator();
                ui.heading("Map overlay");
                overlay_toggled |= ui
                    .checkbox(&mut self.settings.overlay, "Run the DayZ map overlay with the browser")
                    .changed();
                match overlay::program(&self.settings) {
                    Some(path) => {
                        ui.label(RichText::new(path.display().to_string()).weak().small());
                    }
                    None => {
                        ui.label(
                            RichText::new("The overlay (dayz-map) isn't installed or wasn't found.").color(RED),
                        );
                    }
                }
                ui.horizontal(|ui| {
                    ui.label("Program");
                    let edit = ui.add(
                        egui::TextEdit::singleline(&mut window.overlay_program)
                            .hint_text("found automatically")
                            .desired_width(280.0),
                    );
                    if edit.lost_focus() {
                        let text = window.overlay_program.trim();
                        self.settings.overlay_program = (!text.is_empty()).then(|| PathBuf::from(text));
                        overlay_toggled = true;
                    }
                });
                ui.horizontal(|ui| {
                    ui.label("Hotkey in game");
                    if window.capturing {
                        ui.label(RichText::new("Press a key… (Esc cancels)").strong());
                        let pressed = ui.input(|i| {
                            i.events.iter().find_map(|e| match e {
                                egui::Event::Key { key, pressed: true, .. } => Some(*key),
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
                                    "That key can't be used; pick a letter, digit, F1-F24, Tab, or `".into(),
                                    true,
                                ));
                            }
                        }
                    } else {
                        ui.label(RichText::new(overlay::key_label(&window.hotkey)).strong().monospace());
                        if ui.button("Change").clicked() {
                            window.capturing = true;
                            window.hotkey_message = None;
                        }
                    }
                });
                if let Some((message, error)) = &window.hotkey_message {
                    ui.label(if *error { RichText::new(message).color(RED) } else { RichText::new(message).weak() });
                }
            });

        if let Some(key) = test_key {
            window.key_busy = true;
            window.key_message = None;
            self.spawn(move || {
                Msg::KeyTest(
                    steam_api::test_key(&key)
                        .map_err(|e| format!("{e:#}"))
                        .and_then(|_| secret::save(&key).map_err(|e| format!("{e:#}"))),
                )
            });
        }
        if remove_key {
            secret::remove();
            self.key = None;
            window.key_message = Some(("Key removed".into(), false));
            if self.settings.source == Source::Steam {
                self.settings.source = Source::Dzsa;
                source_changed = true;
            }
        }
        if let Some(dir) = use_game_dir {
            match steam::find(dir.as_deref()) {
                Ok(install) => {
                    self.local = LocalMods::load(&install);
                    self.install = Ok(install);
                    self.settings.game_dir = dir;
                    window.game_message = None;
                }
                Err(e) => window.game_message = Some(format!("{e:#}")),
            }
        }
        if let Some(name) = new_hotkey {
            match overlay::set_hotkey(&name) {
                Ok(()) => {
                    window.hotkey = name;
                    let program = overlay::program(&self.settings);
                    if self.overlay == OverlayStatus::Running
                        && let Some(program) = program
                    {
                        window.hotkey_message =
                            Some(("Saved; restarting the overlay to use it".into(), false));
                        self.overlay = OverlayStatus::Checking;
                        self.spawn(move || match overlay::restart(&program) {
                            Ok(()) => {
                                std::thread::sleep(std::time::Duration::from_secs(1));
                                Msg::Overlay(if overlay::is_running(&program) {
                                    OverlayStatus::Running
                                } else {
                                    OverlayStatus::Failed(
                                        "didn't come back after the restart".into(),
                                    )
                                })
                            }
                            Err(e) => Msg::Overlay(OverlayStatus::Failed(format!("{e:#}"))),
                        });
                    } else {
                        window.hotkey_message = Some(("Saved".into(), false));
                    }
                }
                Err(e) => window.hotkey_message = Some((format!("{e:#}"), true)),
            }
        }
        if source_changed {
            // Don't show one list under the other's name while the new one loads.
            self.set_servers(Vec::new());
            self.list.fetched = None;
            self.list.last_request = None;
            self.load_list(false);
        }
        if overlay_toggled {
            self.check_overlay(true);
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
}
