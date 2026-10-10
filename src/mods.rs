//! Which Workshop mods are installed and current, from Steam's own records, and the short links
//! Linux launches use for `-mod`.

#[cfg(not(windows))]
use anyhow::Context;
use anyhow::{Result, bail};
use std::collections::HashMap;
#[cfg(not(windows))]
use std::path::Path;

use crate::steam::Install;
use crate::vdf;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModState {
    Installed,
    /// Installed, but Steam has a newer version to download.
    Outdated,
    Missing,
    /// Not on the Workshop, so the server must provide it some other way.
    NotWorkshop,
}

#[derive(Debug, Clone, Default)]
pub struct LocalItem {
    /// Unix time of the version installed, when Steam recorded it.
    pub time_updated: u64,
    /// Steam knows a newer version (its manifest differs from the latest one).
    pub needs_update: bool,
    /// The Workshop's update time when Steam last compared manifests (its
    /// `latest_timeupdated`), if it recorded one: a Workshop time no newer than this is a change
    /// Steam has already seen, so its manifests say whether the files changed (an edited
    /// description changes the time but not the files).
    pub steam_checked: Option<u64>,
}

/// The mods in DayZ's Workshop folder.
#[derive(Debug, Clone, Default)]
pub struct LocalMods {
    pub items: HashMap<u64, LocalItem>,
}

impl LocalMods {
    pub fn load(install: &Install) -> Self {
        let mut items: HashMap<u64, LocalItem> = HashMap::new();
        if let Ok(entries) = std::fs::read_dir(&install.workshop) {
            for entry in entries.flatten() {
                let Some(id) = entry
                    .file_name()
                    .to_str()
                    .and_then(|n| n.parse::<u64>().ok())
                else {
                    continue;
                };
                // An item mid-download can exist without content yet.
                if entry.path().join("meta.cpp").is_file() || has_addons(&entry.path()) {
                    items.insert(id, LocalItem::default());
                }
            }
        }
        if let Ok(text) = std::fs::read_to_string(&install.manifest) {
            let root = vdf::parse(&text);
            if let Some(app) = root.get("AppWorkshop") {
                let details = app.get("WorkshopItemDetails");
                if let Some(installed) = app.get("WorkshopItemsInstalled") {
                    for (id, entry) in installed.entries() {
                        let Ok(id) = id.parse::<u64>() else { continue };
                        let Some(item) = items.get_mut(&id) else {
                            continue;
                        };
                        item.time_updated = entry
                            .str("timeupdated")
                            .and_then(|t| t.parse().ok())
                            .unwrap_or(0);
                        let manifest = entry.str("manifest");
                        let detail = details.and_then(|d| d.get(&id.to_string()));
                        let latest = detail.and_then(|d| d.str("latest_manifest"));
                        item.needs_update =
                            matches!((manifest, latest), (Some(m), Some(l)) if m != l);
                        item.steam_checked = match (manifest, latest) {
                            (Some(_), Some(_)) => detail
                                .and_then(|d| d.str("latest_timeupdated"))
                                .and_then(|t| t.parse().ok()),
                            _ => None,
                        };
                    }
                }
            }
        }
        Self { items }
    }

    /// A mod's state; `remote_updated` is the Workshop's last update time, if known. Steam's own
    /// record decides when it has seen that update; otherwise an update time newer than the
    /// installed version's counts as outdated.
    pub fn state(&self, id: u64, remote_updated: Option<u64>) -> ModState {
        if id == 0 {
            return ModState::NotWorkshop;
        }
        match self.items.get(&id) {
            None => ModState::Missing,
            Some(item) => {
                let newer_remote = remote_updated.is_some_and(|remote| {
                    let steam_saw_it = item.steam_checked.is_some_and(|seen| remote <= seen);
                    !steam_saw_it && item.time_updated > 0 && remote > item.time_updated
                });
                if item.needs_update || newer_remote {
                    ModState::Outdated
                } else {
                    ModState::Installed
                }
            }
        }
    }
}

/// Whether a mod folder has its `addons` folder, however it's capitalised (mods ship `Addons`
/// as often as `addons`, and Linux folders are case-sensitive).
fn has_addons(dir: &std::path::Path) -> bool {
    std::fs::read_dir(dir).is_ok_and(|entries| {
        entries
            .flatten()
            .any(|e| e.file_name().eq_ignore_ascii_case("addons") && e.path().is_dir())
    })
}

/// The record of the links this browser made (their full paths), so it only ever removes its own.
#[cfg(not(windows))]
const LINKS_FILE: &str = "mod-links.json";

/// Makes a short link in the game folder for each mod (Steam on Linux hangs when launch arguments
/// get long, and full Workshop paths are long) and returns the `-mod` entries in order. Links from
/// earlier launches are removed first. (Linux only: Windows launches use full paths.)
#[cfg(not(windows))]
pub fn link_mods(install: &Install, ids: &[u64], state_dir: &Path) -> Result<Vec<String>> {
    // Check first, so a launch that can't go ahead leaves the folder as it was.
    for &id in ids {
        if !install.workshop.join(id.to_string()).is_dir() {
            bail!("mod {id} isn't downloaded");
        }
    }
    remove_links(install, state_dir);
    let mut made: Vec<std::path::PathBuf> = Vec::new();
    let result = make_links(install, ids, &mut made);
    // Recorded even when a link failed, so the ones made are cleaned up later.
    std::fs::create_dir_all(state_dir).ok();
    std::fs::write(state_dir.join(LINKS_FILE), serde_json::to_vec(&made)?)
        .context("couldn't record the mod links")?;
    result
}

#[cfg(not(windows))]
fn make_links(
    install: &Install,
    ids: &[u64],
    made: &mut Vec<std::path::PathBuf>,
) -> Result<Vec<String>> {
    let mut entries = Vec::new();
    for &id in ids {
        let target = install.workshop.join(id.to_string());
        let name = link_name(id);
        let link = install.game.join(&name);
        if link.symlink_metadata().is_ok() {
            // Left over from a launch whose record was lost: replace it only if it's a link to
            // this same mod.
            if link.read_link().is_ok_and(|to| to == target) {
                std::fs::remove_file(&link).ok();
            } else {
                bail!(
                    "{} already exists and isn't ours; not touching it",
                    link.display()
                );
            }
        }
        symlink_dir(&target, &link)
            .with_context(|| format!("couldn't link {} into the game folder", target.display()))?;
        made.push(link);
        entries.push(name);
    }
    Ok(entries)
}

/// Removes the links recorded by an earlier launch (only ones that are still links).
#[cfg(not(windows))]
pub fn remove_links(install: &Install, state_dir: &Path) {
    let Ok(text) = std::fs::read(state_dir.join(LINKS_FILE)) else {
        return;
    };
    // Older records hold names in the game folder rather than full paths.
    let recorded: Vec<std::path::PathBuf> = serde_json::from_slice(&text).unwrap_or_default();
    for path in recorded {
        let link = if path.is_absolute() {
            path
        } else {
            install.game.join(path)
        };
        let ours = link
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(is_link_name);
        if ours && link.read_link().is_ok() {
            std::fs::remove_file(&link).ok();
        }
    }
    std::fs::remove_file(state_dir.join(LINKS_FILE)).ok();
}

/// `@w` and the id in base 36: short, and unlikely to match a real mod folder.
#[cfg(not(windows))]
pub fn link_name(id: u64) -> String {
    const DIGITS: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    let mut n = id;
    let mut digits = Vec::new();
    loop {
        digits.push(DIGITS[(n % 36) as usize]);
        n /= 36;
        if n == 0 {
            break;
        }
    }
    digits.reverse();
    format!("@w{}", String::from_utf8(digits).unwrap())
}

#[cfg(not(windows))]
fn is_link_name(name: &str) -> bool {
    name.strip_prefix("@w")
        .is_some_and(|rest| !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_alphanumeric()))
}

#[cfg(unix)]
fn symlink_dir(target: &Path, link: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

/// The full path of each mod, for launches that don't need short names (Windows).
#[cfg(windows)]
pub fn mod_paths(install: &Install, ids: &[u64]) -> Result<Vec<std::path::PathBuf>> {
    ids.iter()
        .map(|id| {
            let path = install.workshop.join(id.to_string());
            if path.is_dir() {
                Ok(path)
            } else {
                bail!("mod {id} isn't downloaded")
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn temp_install(name: &str) -> (Install, PathBuf) {
        let root = std::env::temp_dir().join(format!("dzb-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let game = root.join("steamapps/common/DayZ");
        let workshop = root.join("steamapps/workshop/content/221100");
        std::fs::create_dir_all(game.join("Addons")).unwrap();
        std::fs::create_dir_all(&workshop).unwrap();
        let install = Install {
            game,
            workshop,
            manifest: root.join("steamapps/workshop/appworkshop_221100.acf"),
            how: "test",
        };
        (install, root)
    }

    #[test]
    #[cfg(not(windows))]
    fn link_names_are_short_and_ours() {
        assert_eq!(link_name(0), "@w0");
        assert_eq!(link_name(35), "@wz");
        assert_eq!(link_name(1559212036), "@wpsbbpg");
        assert!(link_name(1559212036).len() <= 9);
        assert!(is_link_name("@wpsbbpg"));
        assert!(!is_link_name("@CF"));
        assert!(!is_link_name("@w../x"));
    }

    #[test]
    fn reads_steam_records() {
        let (install, root) = temp_install("records");
        for id in ["111", "222", "333"] {
            std::fs::create_dir_all(install.workshop.join(id).join("addons")).unwrap();
        }
        std::fs::write(
            &install.manifest,
            r#""AppWorkshop" { "WorkshopItemsInstalled" {
                "111" { "timeupdated" "1000" "manifest" "1" }
                "222" { "timeupdated" "1000" "manifest" "1" } }
              "WorkshopItemDetails" { "222" { "latest_manifest" "2" } } }"#,
        )
        .unwrap();
        let local = LocalMods::load(&install);
        assert_eq!(local.state(111, None), ModState::Installed);
        assert_eq!(local.state(111, Some(2000)), ModState::Outdated);
        assert_eq!(local.state(222, None), ModState::Outdated);
        assert_eq!(
            local.state(333, Some(5)),
            ModState::Installed,
            "no record: trust the folder"
        );
        assert_eq!(local.state(444, None), ModState::Missing);
        assert_eq!(local.state(0, None), ModState::NotWorkshop);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn finds_addons_in_any_case() {
        let (install, root) = temp_install("addons-case");
        std::fs::create_dir_all(install.workshop.join("111/Addons")).unwrap();
        std::fs::create_dir_all(install.workshop.join("222/ADDONS")).unwrap();
        // Mid-download: neither content nor meta.cpp yet.
        std::fs::create_dir_all(install.workshop.join("333/keys")).unwrap();
        // A file by that name isn't the folder.
        std::fs::create_dir_all(install.workshop.join("444")).unwrap();
        std::fs::write(install.workshop.join("444/addons"), "").unwrap();
        let local = LocalMods::load(&install);
        assert_eq!(local.state(111, None), ModState::Installed);
        assert_eq!(local.state(222, None), ModState::Installed);
        assert_eq!(local.state(333, None), ModState::Missing);
        assert_eq!(local.state(444, None), ModState::Missing);
        std::fs::remove_dir_all(root).unwrap();
    }

    /// Steam's record as it writes it (tabs, and both sections).
    #[test]
    fn prefers_steams_manifests_to_update_times() {
        let (install, root) = temp_install("acf");
        for id in ["1559212036", "1564026768", "1565871491", "1566911166"] {
            std::fs::create_dir_all(install.workshop.join(id).join("addons")).unwrap();
        }
        std::fs::write(
            &install.manifest,
            "\"AppWorkshop\"\n{\n\t\"appid\"\t\t\"221100\"\n\t\"NeedsUpdate\"\t\t\"0\"\n\
             \t\"WorkshopItemsInstalled\"\n\t{\n\
             \t\t\"1559212036\"\n\t\t{\n\t\t\t\"size\"\t\t\"527656\"\n\
             \t\t\t\"timeupdated\"\t\t\"1771519119\"\n\t\t\t\"manifest\"\t\t\"4114705373119672275\"\n\t\t}\n\
             \t\t\"1564026768\"\n\t\t{\n\t\t\t\"timeupdated\"\t\t\"1784113051\"\n\
             \t\t\t\"manifest\"\t\t\"4299442315021813277\"\n\t\t}\n\
             \t\t\"1565871491\"\n\t\t{\n\t\t\t\"timeupdated\"\t\t\"1732651888\"\n\
             \t\t\t\"manifest\"\t\t\"2642877461524886469\"\n\t\t}\n\
             \t\t\"1566911166\"\n\t\t{\n\t\t\t\"timeupdated\"\t\t\"1775676235\"\n\
             \t\t\t\"manifest\"\t\t\"1852252921219504809\"\n\t\t}\n\
             \t}\n\
             \t\"WorkshopItemDetails\"\n\t{\n\
             \t\t\"1559212036\"\n\t\t{\n\t\t\t\"manifest\"\t\t\"4114705373119672275\"\n\
             \t\t\t\"timeupdated\"\t\t\"1771519119\"\n\t\t\t\"timetouched\"\t\t\"1791506075\"\n\
             \t\t\t\"latest_timeupdated\"\t\t\"1780000000\"\n\
             \t\t\t\"latest_manifest\"\t\t\"4114705373119672275\"\n\t\t}\n\
             \t\t\"1564026768\"\n\t\t{\n\t\t\t\"manifest\"\t\t\"4299442315021813277\"\n\
             \t\t\t\"latest_timeupdated\"\t\t\"1790000000\"\n\
             \t\t\t\"latest_manifest\"\t\t\"5000000000000000000\"\n\t\t}\n\
             \t\t\"1565871491\"\n\t\t{\n\t\t\t\"manifest\"\t\t\"2642877461524886469\"\n\
             \t\t\t\"latest_timeupdated\"\t\t\"1732651888\"\n\
             \t\t\t\"latest_manifest\"\t\t\"2642877461524886469\"\n\t\t}\n\
             \t}\n}\n",
        )
        .unwrap();
        let local = LocalMods::load(&install);
        // Only the description changed: Steam saw that update, and the files are the same.
        assert_eq!(
            local.state(1559212036, Some(1780000000)),
            ModState::Installed
        );
        // Steam knows of new files.
        assert_eq!(local.state(1564026768, None), ModState::Outdated);
        assert_eq!(
            local.state(1564026768, Some(1784113051)),
            ModState::Outdated
        );
        // An update newer than Steam last saw: Steam's record is stale, so the times decide.
        assert_eq!(
            local.state(1565871491, Some(1732651888)),
            ModState::Installed
        );
        assert_eq!(
            local.state(1565871491, Some(1790000000)),
            ModState::Outdated
        );
        // No details from Steam: the times decide.
        assert_eq!(
            local.state(1566911166, Some(1775676235)),
            ModState::Installed
        );
        assert_eq!(
            local.state(1566911166, Some(1775676236)),
            ModState::Outdated
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    #[cfg(unix)]
    fn links_are_replaced_and_cleaned_up() {
        let (install, root) = temp_install("links");
        let state = root.join("state");
        for id in [10u64, 20] {
            std::fs::create_dir_all(install.workshop.join(id.to_string())).unwrap();
        }
        let first = link_mods(&install, &[10, 20], &state).unwrap();
        assert_eq!(first, vec![link_name(10), link_name(20)]);
        assert!(install.game.join(link_name(10)).join(".").exists());
        // A later launch with fewer mods drops the old links.
        link_mods(&install, &[20], &state).unwrap();
        assert!(install.game.join(link_name(10)).symlink_metadata().is_err());
        // A missing mod stops the launch before anything changes.
        assert!(
            link_mods(&install, &[20, 99], &state).is_err(),
            "missing mod"
        );
        assert!(install.game.join(link_name(20)).join(".").exists());
        // A real folder with a link's name is left alone, and the links made are still recorded.
        std::fs::create_dir_all(install.game.join(link_name(30))).unwrap();
        std::fs::create_dir_all(install.workshop.join("30")).unwrap();
        assert!(link_mods(&install, &[20, 30], &state).is_err());
        assert!(install.game.join(link_name(30)).is_dir());
        remove_links(&install, &state);
        assert!(install.game.join(link_name(20)).symlink_metadata().is_err());
        assert!(install.game.join(link_name(30)).is_dir());
        std::fs::remove_dir_all(root).unwrap();
    }
}
