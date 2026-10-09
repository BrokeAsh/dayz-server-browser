//! Steam's install, its library folders, and where DayZ and its Workshop mods are.

use anyhow::{Result, bail};
use std::path::{Path, PathBuf};

use crate::vdf;

/// DayZ's Steam app id.
pub const DAYZ_APP: u32 = 221100;

/// Where DayZ and its Workshop mods are on this machine.
#[derive(Debug, Clone)]
pub struct Install {
    /// `<library>/steamapps/common/DayZ`.
    pub game: PathBuf,
    /// `<library>/steamapps/workshop/content/221100`, one folder per mod (it may not exist yet).
    pub workshop: PathBuf,
    /// `<library>/steamapps/workshop/appworkshop_221100.acf`: Steam's record of installed mods.
    pub manifest: PathBuf,
    /// How the game folder was found, for the settings window.
    pub how: &'static str,
}

/// Finds DayZ: the folder from the settings if it's a DayZ install, otherwise through Steam's
/// library list.
pub fn find(configured: Option<&Path>) -> Result<Install> {
    if let Some(dir) = configured {
        if is_game_dir(dir) {
            return Ok(install(dir.to_path_buf(), "settings"));
        }
        bail!("{} isn't a DayZ folder", dir.display());
    }
    for library in library_folders() {
        let game = library.join("steamapps").join("common").join("DayZ");
        if is_game_dir(&game) {
            return Ok(install(game, "Steam library"));
        }
    }
    bail!("DayZ wasn't found in any Steam library; set its folder in Settings")
}

fn install(game: PathBuf, how: &'static str) -> Install {
    // Workshop mods go in the library the game is in.
    let steamapps = game
        .parent()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .unwrap_or_default();
    let workshop = steamapps.join("workshop");
    Install {
        game,
        workshop: workshop.join("content").join(DAYZ_APP.to_string()),
        manifest: workshop.join(format!("appworkshop_{DAYZ_APP}.acf")),
        how,
    }
}

/// True for a DayZ install folder.
pub fn is_game_dir(dir: &Path) -> bool {
    dir.join("Addons").is_dir()
        && (dir.join("DayZ_x64.exe").is_file() || dir.join("DayZ_BE.exe").is_file())
}

/// Where Steam itself may be installed, with whether it's the Flatpak package.
#[cfg(not(windows))]
pub fn steam_roots() -> Vec<(PathBuf, bool)> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    vec![
        (home.join(".local/share/Steam"), false),
        (home.join(".steam/steam"), false),
        (home.join(".steam/root"), false),
        (home.join(".steam/debian-installation"), false),
        (
            home.join(".var/app/com.valvesoftware.Steam/.local/share/Steam"),
            true,
        ),
        (
            home.join(".var/app/com.valvesoftware.Steam/data/Steam"),
            true,
        ),
        (home.join("snap/steam/common/.local/share/Steam"), false),
    ]
}

/// Where Steam itself may be installed.
#[cfg(windows)]
pub fn steam_roots() -> Vec<(PathBuf, bool)> {
    let mut roots: Vec<(PathBuf, bool)> = crate::win::steam_path()
        .map(|p| (PathBuf::from(p.replace('/', "\\")), false))
        .into_iter()
        .collect();
    roots.push((PathBuf::from(r"C:\Program Files (x86)\Steam"), false));
    roots
}

/// Every Steam library folder (each has a `steamapps` folder), from each Steam install's
/// `libraryfolders.vdf`.
pub fn library_folders() -> Vec<PathBuf> {
    let mut libraries = Vec::new();
    for (root, _) in steam_roots() {
        let vdf = root.join("steamapps").join("libraryfolders.vdf");
        if let Ok(text) = std::fs::read_to_string(&vdf) {
            let parsed = vdf::parse(&text);
            if let Some(folders) = parsed.get("libraryfolders") {
                for (_, folder) in folders.entries() {
                    if let Some(path) = folder.str("path") {
                        libraries.push(PathBuf::from(path));
                    }
                }
            }
        }
        libraries.push(root);
    }
    dedup_dirs(libraries)
}

/// Keeps existing folders, each once (`~/.steam/steam` usually links to `~/.local/share/Steam`).
fn dedup_dirs(dirs: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut seen = std::collections::HashSet::new();
    dirs.into_iter()
        .filter(|d| d.is_dir() && seen.insert(d.canonicalize().unwrap_or_else(|_| d.clone())))
        .collect()
}

/// How to run Steam on Linux: the `steam` command, or the Flatpak package when only that is
/// installed.
#[cfg(not(windows))]
pub fn steam_command() -> Result<Vec<String>> {
    if which("steam").is_some() {
        return Ok(vec!["steam".into()]);
    }
    let flatpak = steam_roots()
        .into_iter()
        .any(|(root, flatpak)| flatpak && root.is_dir());
    if flatpak && which("flatpak").is_some() {
        return Ok(["flatpak", "run", "com.valvesoftware.Steam"]
            .map(String::from)
            .to_vec());
    }
    bail!("Steam wasn't found (neither the steam command nor the Flatpak)")
}

/// The first `name` on `PATH`.
#[cfg(not(windows))]
fn which(name: &str) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|dir| dir.join(name))
        .find(|p| p.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workshop_follows_the_game_library() {
        let found = install(PathBuf::from("/games/lib/steamapps/common/DayZ"), "test");
        assert_eq!(
            found.workshop,
            Path::new("/games/lib/steamapps/workshop/content/221100")
        );
        assert_eq!(
            found.manifest,
            Path::new("/games/lib/steamapps/workshop/appworkshop_221100.acf")
        );
    }
}
