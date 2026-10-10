//! The browser's settings (`config.toml`) and what it remembers between runs (`state.json`:
//! filters, favorites, recent servers).

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use crate::server::{Filters, SortBy};

pub fn dirs() -> directories::ProjectDirs {
    directories::ProjectDirs::from("", "", "dayz-browser").expect("home directory is known")
}

pub fn config_dir() -> PathBuf {
    dirs().config_dir().to_path_buf()
}

pub fn data_dir() -> PathBuf {
    dirs().data_dir().to_path_buf()
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    /// DZSA Launcher's public list.
    #[default]
    Dzsa,
    /// Steam's list, with the user's own Web API key.
    Steam,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub source: Source,
    /// Added to every launch, written as on a command line (quotes allowed).
    pub extra_args: String,
    /// Run the DayZ map overlay alongside the game.
    pub overlay: bool,
    /// The DayZ folder; found through Steam when unset.
    pub game_dir: Option<PathBuf>,
    /// Look for a new version of the browser at start.
    pub check_updates: bool,
    /// Show the server being played in Discord (when the browser has a Discord application).
    pub discord_presence: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            source: Source::Dzsa,
            extra_args: String::new(),
            overlay: true,
            game_dir: None,
            check_updates: true,
            discord_presence: true,
        }
    }
}

impl Settings {
    pub fn load() -> Self {
        let path = config_dir().join("config.toml");
        match std::fs::read_to_string(&path) {
            Ok(text) => toml::from_str(&text).unwrap_or_else(|e| {
                set_aside(&path, &e);
                Self::default()
            }),
            Err(_) => Self::default(),
        }
    }

    pub fn save(&self) -> anyhow::Result<()> {
        write_atomic(
            &config_dir().join("config.toml"),
            toml::to_string_pretty(self)?.as_bytes(),
        )
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Recent {
    pub addr: String,
    pub name: String,
    pub map: String,
    /// Unix time it was joined.
    pub joined: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct State {
    pub filters: Filters,
    pub sort: SortBy,
    pub sort_descending: bool,
    /// `ip:query_port` of each favorite.
    pub favorites: Vec<String>,
    /// Most recent first.
    pub recent: Vec<Recent>,
    /// Widths of the list's Map, Players, Queue, Time, Ping, and Mods columns; the server name
    /// takes the rest.
    #[serde(deserialize_with = "columns")]
    pub columns: [f32; 6],
    /// Servers added by address (`ip:query_port`), which the list may not include.
    pub added: Vec<String>,
    /// The date (Unix time) of the newest DayZ news post the user has seen.
    #[serde(deserialize_with = "lenient_u64")]
    pub news_seen: u64,
}

pub const COLUMNS: [f32; 6] = [100.0, 64.0, 48.0, 48.0, 44.0, 44.0];

/// Reads the column widths, including those saved before the Queue column (which gets its
/// default); anything else odd gives the defaults rather than failing the whole state.
fn columns<'de, D: serde::Deserializer<'de>>(d: D) -> Result<[f32; 6], D::Error> {
    let value = serde_json::Value::deserialize(d)?;
    let widths: Vec<f32> = serde_json::from_value(value).unwrap_or_default();
    Ok(match widths[..] {
        [map, players, queue, time, ping, mods] => [map, players, queue, time, ping, mods],
        [map, players, time, ping, mods] => [map, players, COLUMNS[2], time, ping, mods],
        _ => COLUMNS,
    })
}

/// Reads a number, giving 0 for anything else rather than failing the whole state.
fn lenient_u64<'de, D: serde::Deserializer<'de>>(d: D) -> Result<u64, D::Error> {
    let value = serde_json::Value::deserialize(d)?;
    Ok(value.as_u64().unwrap_or(0))
}

impl Default for State {
    fn default() -> Self {
        Self {
            filters: Filters::default(),
            sort: SortBy::Players,
            sort_descending: true,
            favorites: Vec::new(),
            recent: Vec::new(),
            columns: COLUMNS,
            added: Vec::new(),
            news_seen: 0,
        }
    }
}

impl State {
    pub fn load() -> Self {
        let path = data_dir().join("state.json");
        match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|e| {
                set_aside(&path, &e);
                Self::default()
            }),
            Err(_) => Self::default(),
        }
    }

    pub fn save(&self) -> anyhow::Result<()> {
        write_atomic(
            &data_dir().join("state.json"),
            &serde_json::to_vec_pretty(self)?,
        )
    }

    pub fn is_favorite(&self, addr: &str) -> bool {
        self.favorites.iter().any(|f| f == addr)
    }

    pub fn toggle_favorite(&mut self, addr: &str) {
        if let Some(i) = self.favorites.iter().position(|f| f == addr) {
            self.favorites.remove(i);
        } else {
            self.favorites.push(addr.to_string());
        }
    }

    pub fn add_recent(&mut self, recent: Recent) {
        self.recent.retain(|r| r.addr != recent.addr);
        self.recent.insert(0, recent);
        self.recent.truncate(20);
    }
}

/// Writes through a temporary file, so a crash can't leave half a file.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    use std::io::Write;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension(format!("{}.tmp", std::process::id()));
    let written = std::fs::File::create(&tmp)
        .and_then(|mut file| {
            file.write_all(bytes)?;
            // On disk before it replaces the old file, so a crash can't leave an empty one.
            file.sync_all()
        })
        .and_then(|()| std::fs::rename(&tmp, path));
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    Ok(written?)
}

/// Moves an unreadable file aside (as `<name>.bad`) so saving defaults doesn't destroy it.
fn set_aside(path: &Path, error: &dyn std::fmt::Display) {
    let bad = path.with_extension(format!(
        "{}.bad",
        path.extension()
            .and_then(|e| e.to_str())
            .unwrap_or_default()
    ));
    log::warn!(
        "{} is unreadable ({error}); starting over and keeping it as {}",
        path.display(),
        bad.display()
    );
    let _ = std::fs::rename(path, bad);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_older_column_widths() {
        let state: State =
            serde_json::from_str(r#"{"favorites":["1.2.3.4:2303"],"columns":[200,80,48,64,44]}"#)
                .unwrap();
        assert_eq!(state.columns, [200.0, 80.0, COLUMNS[2], 48.0, 64.0, 44.0]);
        assert_eq!(state.favorites, ["1.2.3.4:2303"]);
        let state: State =
            serde_json::from_str(r#"{"favorites":["1.2.3.4:2303"],"columns":"wide"}"#).unwrap();
        assert_eq!(state.columns, COLUMNS);
        assert_eq!(state.favorites, ["1.2.3.4:2303"]);
    }

    #[test]
    fn reads_state_from_before_the_news() {
        let state: State = serde_json::from_str(r#"{"favorites":["1.2.3.4:2303"]}"#).unwrap();
        assert_eq!(state.news_seen, 0);
        assert_eq!(state.favorites, ["1.2.3.4:2303"]);
        let state: State =
            serde_json::from_str(r#"{"favorites":["1.2.3.4:2303"],"news_seen":"soon"}"#).unwrap();
        assert_eq!(state.news_seen, 0);
        assert_eq!(state.favorites, ["1.2.3.4:2303"]);
        let state = State {
            news_seen: 1791555988,
            ..State::default()
        };
        let again: State = serde_json::from_slice(&serde_json::to_vec(&state).unwrap()).unwrap();
        assert_eq!(again.news_seen, 1791555988);
    }

    #[test]
    fn reads_settings_from_before_discord() {
        let settings: Settings = toml::from_str("overlay = false\n").unwrap();
        assert!(settings.discord_presence, "on unless turned off");
        assert!(!settings.overlay);
        let settings: Settings = toml::from_str("discord_presence = false\n").unwrap();
        assert!(!settings.discord_presence);
    }
}
