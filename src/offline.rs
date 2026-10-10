//! Offline mode: DayZ on a map of your own, with no server, through the Community Offline Mode
//! missions (<https://github.com/Arkensor/DayZCommunityOfflineMode>). They aren't a Workshop mod
//! but a GitHub release: a zip of mission folders (`Missions/DayZCommunityOfflineMode.<world>`),
//! which the browser keeps in its data folder. Each map's offline save is in its mission folder
//! (`storage_-1`), and survives updates of the release.
//!
//! - Linux: Steam starts the game (under Proton) from its own folder, so the browser links
//!   `<DayZ>/Missions` to its copy and passes `-mission=Missions/<folder>`, as DZGUI does. The
//!   link is recorded and replaced only if it's the browser's own, like the mod links.
//! - Windows: `DayZ_x64.exe -mission=<full path>`, without BattlEye, as the release's own `.bat`
//!   does. A mission folder can be anywhere when its full path is given, so nothing is written to
//!   the game folder.

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use crate::launch::Launch;
use crate::mods;
use crate::server::map_title;
use crate::settings::{data_dir, write_atomic};
use crate::steam::Install;

const LATEST: &str =
    "https://api.github.com/repos/Arkensor/DayZCommunityOfflineMode/releases/latest";
/// The project's page, for the window's link.
pub const PROJECT: &str = "https://github.com/Arkensor/DayZCommunityOfflineMode";
/// The release's download, by name; any other zip is the fallback.
const ASSET: &str = "DayZ.Community.OfflineMode.zip";
/// Far more than the release needs (13 MB); a bigger download is something else.
const MAX_DOWNLOAD: u64 = 500_000_000;

/// In the offline folder: which release is unpacked, and the missions.
const RECORD: &str = "release.json";
const MISSIONS: &str = "Missions";
/// While installing: the new release, the previous missions, and the download.
const NEW: &str = ".new";
const OLD: &str = ".old";
const DOWNLOAD: &str = ".download.zip";
/// The offline save, inside each mission folder.
const SAVE: &str = "storage_-1";

/// The Workshop mods the Namalsk mission needs, in load order: Namalsk Island and Namalsk
/// Survival.
pub const NAMALSK_MODS: [u64; 2] = [2289456201, 2289461232];

/// What the release's `.bat` starts the game with, after `-mission`.
const GAME_ARGS: [&str; 6] = [
    "-nosplash",
    "-noPause",
    "-noBenchmark",
    "-filePatching",
    "-doLogs",
    "-scriptDebug=true",
];

/// Where the browser keeps the release.
pub fn dir() -> PathBuf {
    data_dir().join("offline")
}

/// A release of Community Offline Mode, and its download. The one unpacked is recorded as this.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Release {
    pub tag: String,
    /// The release's page.
    pub page: String,
    pub asset_id: u64,
    pub size: u64,
    /// When the download was last replaced (GitHub's `updated_at`).
    pub updated: String,
    pub url: String,
    /// The download's SHA-256, when GitHub lists one.
    #[serde(default)]
    pub sha256: Option<String>,
}

#[derive(Deserialize)]
struct GithubRelease {
    tag_name: String,
    html_url: String,
    #[serde(default)]
    assets: Vec<GithubAsset>,
}

#[derive(Deserialize)]
struct GithubAsset {
    id: u64,
    name: String,
    size: u64,
    #[serde(default)]
    updated_at: String,
    browser_download_url: String,
    /// Such as `sha256:…`; only on newer uploads.
    #[serde(default)]
    digest: Option<String>,
}

/// Asks GitHub for the latest release.
pub fn latest() -> Result<Release> {
    // For testing against a stand-in release.
    let url = std::env::var("DAYZ_BROWSER_OFFLINE_URL").unwrap_or_else(|_| LATEST.into());
    let release: GithubRelease = crate::update::agent()
        .get(&url)
        .header("Accept", "application/vnd.github+json")
        .call()
        .context("couldn't ask GitHub for the Offline Mode release")?
        .body_mut()
        .read_json()
        .context("GitHub's answer about the Offline Mode release was unreadable")?;
    pick(release)
}

fn pick(release: GithubRelease) -> Result<Release> {
    let asset = release
        .assets
        .iter()
        .find(|a| a.name == ASSET)
        .or_else(|| release.assets.iter().find(|a| a.name.ends_with(".zip")))
        .context("the latest Offline Mode release has no download")?;
    Ok(Release {
        tag: release.tag_name.clone(),
        page: release.html_url.clone(),
        asset_id: asset.id,
        size: asset.size,
        updated: asset.updated_at.clone(),
        url: asset.browser_download_url.clone(),
        sha256: asset
            .digest
            .as_deref()
            .and_then(|d| d.strip_prefix("sha256:"))
            .map(str::to_lowercase),
    })
}

/// Whether `latest` differs from the release unpacked (or nothing is), so it's worth getting.
pub fn changed(installed: Option<&Release>, latest: &Release) -> bool {
    installed.is_none_or(|i| {
        i.tag != latest.tag
            || i.asset_id != latest.asset_id
            || i.size != latest.size
            || i.updated != latest.updated
    })
}

/// The release unpacked in `dir`, if its missions are there.
pub fn installed(dir: &Path) -> Option<Release> {
    if !dir.join(MISSIONS).is_dir() {
        return None;
    }
    let text = std::fs::read(dir.join(RECORD)).ok()?;
    serde_json::from_slice(&text).ok()
}

/// One map's mission folder.
#[derive(Debug, Clone, PartialEq)]
pub struct Mission {
    /// The folder's name, such as `DayZCommunityOfflineMode.ChernarusPlus`.
    pub folder: String,
    /// Its full path.
    pub path: PathBuf,
    /// The map's name, such as `Livonia`.
    pub title: String,
    /// There's an offline save to continue.
    pub saved: bool,
    /// Workshop mods it needs, in load order.
    pub mods: Vec<u64>,
}

/// The world a mission folder is for (`chernarusplus` for `….ChernarusPlus`), if it's a mission:
/// named `<name>.<world>`, with an `init.c`.
pub fn mission_world(path: &Path) -> Option<String> {
    let name = path.file_name()?.to_str()?;
    let (stem, world) = name.rsplit_once('.')?;
    let usable =
        |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    if !(usable(stem) && usable(world)) || !path.join("init.c").is_file() {
        return None;
    }
    Some(world.to_ascii_lowercase())
}

/// The missions in a `Missions` folder: Chernarus, Livonia, and Namalsk first, then any others.
pub fn missions(folder: &Path) -> Vec<Mission> {
    let Ok(entries) = std::fs::read_dir(folder) else {
        return Vec::new();
    };
    let mut found: Vec<(usize, Mission)> = entries
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .filter_map(|e| {
            let path = e.path();
            let world = mission_world(&path)?;
            let order = ["chernarusplus", "enoch", "namalsk"]
                .iter()
                .position(|w| *w == world)
                .unwrap_or(usize::MAX);
            Some((
                order,
                Mission {
                    folder: e.file_name().to_str()?.to_string(),
                    title: map_title(&world),
                    saved: path.join(SAVE).is_dir(),
                    mods: if world == "namalsk" {
                        NAMALSK_MODS.to_vec()
                    } else {
                        Vec::new()
                    },
                    path,
                },
            ))
        })
        .collect();
    found.sort_by(|(a, x), (b, y)| a.cmp(b).then_with(|| x.title.cmp(&y.title)));
    found.into_iter().map(|(_, m)| m).collect()
}

/// The missions of the release unpacked in `dir`.
pub fn installed_missions(dir: &Path) -> Vec<Mission> {
    missions(&dir.join(MISSIONS))
}

/// Downloads `release` into `dir` and puts it in place of the one there, keeping the saves.
/// Reports bytes so far and the total. Nothing changes if the download fails.
pub fn download(release: &Release, dir: &Path, mut progress: impl FnMut(u64, u64)) -> Result<()> {
    if release.size > MAX_DOWNLOAD {
        bail!("the Offline Mode download is unexpectedly large; not downloading it");
    }
    std::fs::create_dir_all(dir).with_context(|| format!("couldn't create {}", dir.display()))?;
    let zip = dir.join(DOWNLOAD);
    let result = fetch(release, &zip, &mut progress).and_then(|()| install(dir, &zip, release));
    let _ = std::fs::remove_file(&zip);
    result
}

fn fetch(release: &Release, to: &Path, progress: &mut impl FnMut(u64, u64)) -> Result<()> {
    let mut response = crate::update::agent()
        .get(&release.url)
        .call()
        .context("couldn't download Offline Mode from GitHub")?;
    let mut body = response.body_mut().as_reader();
    let mut file =
        std::fs::File::create(to).with_context(|| format!("couldn't write {}", to.display()))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    let mut done = 0u64;
    loop {
        let n = body.read(&mut buf).context("the download broke off")?;
        if n == 0 {
            break;
        }
        done += n as u64;
        if done > MAX_DOWNLOAD {
            bail!("the Offline Mode download is unexpectedly large; stopped");
        }
        hasher.update(&buf[..n]);
        file.write_all(&buf[..n])
            .with_context(|| format!("couldn't write {}", to.display()))?;
        progress(done, release.size);
    }
    file.sync_all()?;
    if release.size > 0 && done != release.size {
        bail!("the download broke off ({done} of {} bytes)", release.size);
    }
    if let Some(expected) = &release.sha256 {
        let actual: String = hasher
            .finalize()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        if &actual != expected {
            bail!("the download doesn't match GitHub's checksum");
        }
    }
    Ok(())
}

/// Unpacks a downloaded release and swaps its missions in, then records it.
fn install(dir: &Path, zip: &Path, release: &Release) -> Result<()> {
    let staging = dir.join(NEW);
    let _ = std::fs::remove_dir_all(&staging);
    let result = (|| {
        let file = std::fs::File::open(zip)?;
        let mut archive =
            zip::ZipArchive::new(file).context("couldn't read the Offline Mode download")?;
        check_archive(&mut archive)?;
        // `extract` keeps every entry inside `staging`.
        archive
            .extract(&staging)
            .context("couldn't unpack the Offline Mode download")?;
        let missions = staging.join(MISSIONS);
        if self::missions(&missions).is_empty() {
            bail!("the Offline Mode download doesn't hold any missions");
        }
        swap_in(dir, &missions)?;
        write_atomic(&dir.join(RECORD), &serde_json::to_vec_pretty(release)?)
    })();
    let _ = std::fs::remove_dir_all(&staging);
    result
}

/// Far more than the release unpacks to (a few tens of MB); a zip that says it holds more is
/// something else, and isn't unpacked.
const MAX_UNPACKED: u64 = 1_000_000_000;
const MAX_ENTRIES: usize = 10_000;

/// Checks, from a zip's directory, that it unpacks to a sensible size and number of files, and
/// holds only files and folders (no links). Unpacking reads no more of an entry than its listed
/// size, so this bounds what it writes.
fn check_archive<R: std::io::Read + std::io::Seek>(archive: &mut zip::ZipArchive<R>) -> Result<()> {
    check_within(archive, MAX_ENTRIES, MAX_UNPACKED)
}

fn check_within<R: std::io::Read + std::io::Seek>(
    archive: &mut zip::ZipArchive<R>,
    max_entries: usize,
    max_unpacked: u64,
) -> Result<()> {
    if archive.len() > max_entries {
        bail!(
            "the Offline Mode download holds {} files, unexpectedly many; not unpacking it",
            archive.len()
        );
    }
    let mut total = 0u64;
    for i in 0..archive.len() {
        let entry = archive
            .by_index_raw(i)
            .context("couldn't read the Offline Mode download")?;
        if entry.is_symlink() {
            bail!("the Offline Mode download holds a link; not unpacking it");
        }
        total = total.saturating_add(entry.size());
    }
    if total > max_unpacked {
        bail!("the Offline Mode download unpacks to unexpectedly much; not unpacking it");
    }
    Ok(())
}

/// Puts the missions in `new` in place of `dir/Missions`, carrying the offline saves (and any
/// mission the new release no longer has) over from the old ones.
fn swap_in(dir: &Path, new: &Path) -> Result<()> {
    let current = dir.join(MISSIONS);
    let old = dir.join(OLD);
    // Left by an install that stopped part-way: its saves come first.
    restore_saves(dir)
        .with_context(|| format!("an earlier update left offline saves in {}", old.display()))?;
    let had_current = current.exists();
    if had_current {
        std::fs::rename(&current, &old)
            .with_context(|| format!("couldn't move {} aside", current.display()))?;
    }
    if let Err(e) = std::fs::rename(new, &current) {
        if had_current {
            let _ = std::fs::rename(&old, &current);
        }
        return Err(e).with_context(|| format!("couldn't put the new {} in place", MISSIONS));
    }
    if had_current {
        carry_over(&old, &current, Keep::Moved).with_context(|| {
            format!(
                "the update is in place, but some offline saves are still in {}",
                old.display()
            )
        })?;
        std::fs::remove_dir_all(&old)
            .with_context(|| format!("couldn't remove {}", old.display()))?;
    }
    Ok(())
}

/// Where an update keeps the previous missions while it carries their saves over.
pub fn waiting_dir(dir: &Path) -> PathBuf {
    dir.join(OLD)
}

/// The maps (mission folders) whose saves an update left in [`waiting_dir`], because it couldn't
/// move them back; [`restore_saves`] tries again.
pub fn waiting_saves(dir: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(waiting_dir(dir)) else {
        return Vec::new();
    };
    let mut waiting: Vec<String> = entries
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .filter(|e| saves_in(&e.path()).next().is_some())
        .filter_map(|e| e.file_name().to_str().map(String::from))
        .collect();
    waiting.sort();
    waiting
}

/// Moves the saves an earlier update left in `dir/.old` back into the missions, and removes what
/// else is there (the previous release's files). Where a map has a save in both, the newer one
/// is played and the other kept beside it as `storage_-1.bak`. Returns the maps (mission
/// folders) where that happened.
pub fn restore_saves(dir: &Path) -> Result<Vec<String>> {
    let current = dir.join(MISSIONS);
    let old = dir.join(OLD);
    if !old.exists() {
        return Ok(Vec::new());
    }
    if !current.exists() {
        std::fs::rename(&old, &current)
            .with_context(|| format!("couldn't move {} back", old.display()))?;
        return Ok(Vec::new());
    }
    let set_aside = carry_over(&old, &current, Keep::Newer)?;
    std::fs::remove_dir_all(&old).with_context(|| format!("couldn't remove {}", old.display()))?;
    Ok(set_aside)
}

/// Which save stays in place when a mission has one on both sides of [`carry_over`]. The other is
/// never deleted, only set aside.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Keep {
    /// The one moved in: the user's save over one a release might ship.
    Moved,
    /// The one changed last (the one already in place when that can't be told).
    Newer,
}

/// Moves each mission's saves (and saves set aside earlier) from `from` into the same mission in
/// `to`, and missions `to` lacks entirely. Returns the missions where two saves met and one was
/// set aside.
fn carry_over(from: &Path, to: &Path, keep: Keep) -> Result<Vec<String>> {
    let mut failed = Vec::new();
    let mut set_aside = Vec::new();
    for entry in std::fs::read_dir(from)?.flatten() {
        if !entry.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        let target = to.join(entry.file_name());
        let moved = if !target.exists() {
            std::fs::rename(entry.path(), &target)
        } else {
            let saves: Vec<PathBuf> = saves_in(&entry.path()).collect();
            saves.iter().try_for_each(|save| {
                if save.file_name() != Some(SAVE.as_ref()) {
                    // One set aside before: it stays set aside.
                    return std::fs::rename(save, spare_name(&target));
                }
                let (aside, met) = move_save(save, &target, keep)?;
                if met {
                    log::info!("two offline saves for {name}; kept {}", aside.display());
                    set_aside.push(name.clone());
                }
                Ok(())
            })
        };
        if moved.is_err() {
            failed.push(name);
        }
    }
    if !failed.is_empty() {
        bail!("couldn't move {}", failed.join(", "));
    }
    Ok(set_aside)
}

/// Moves a mission's `save` into the mission folder `target`. If that has a save already, the
/// one `keep` says stays as `storage_-1` and the other becomes a `storage_-1.bak` beside it.
/// Returns where a save was set aside, and whether one was.
fn move_save(save: &Path, target: &Path, keep: Keep) -> std::io::Result<(PathBuf, bool)> {
    let dest = target.join(SAVE);
    if !dest.exists() {
        std::fs::rename(save, &dest)?;
        return Ok((dest, false));
    }
    let moved_wins = match keep {
        Keep::Moved => true,
        Keep::Newer => match (last_change(save), last_change(&dest)) {
            (Some(moved), Some(there)) => moved > there,
            (Some(_), None) => true,
            _ => false,
        },
    };
    let spare = spare_name(target);
    if moved_wins {
        std::fs::rename(&dest, &spare)?;
        std::fs::rename(save, &dest)?;
    } else {
        std::fs::rename(save, &spare)?;
    }
    Ok((spare, true))
}

/// A mission folder's save and the saves set aside beside it.
fn saves_in(mission: &Path) -> impl Iterator<Item = PathBuf> {
    std::fs::read_dir(mission)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| {
            e.file_name()
                .to_str()
                .is_some_and(|n| n == SAVE || n.starts_with(&format!("{SAVE}.bak")))
        })
        .map(|e| e.path())
}

/// A free name in a mission folder for a save set aside: `storage_-1.bak`, then
/// `storage_-1.bak2`, and so on.
fn spare_name(mission: &Path) -> PathBuf {
    let mut n = 1;
    loop {
        let name = if n == 1 {
            format!("{SAVE}.bak")
        } else {
            format!("{SAVE}.bak{n}")
        };
        let path = mission.join(name);
        if path.symlink_metadata().is_err() {
            return path;
        }
        n += 1;
    }
}

/// When anything in a save last changed: the newest modification time of what's in it (not of
/// the folder itself, which moving it may touch). `None` for an empty save.
fn last_change(save: &Path) -> Option<std::time::SystemTime> {
    let mut newest = None;
    let mut stack = vec![save.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let Ok(meta) = entry.metadata() else { continue };
            if meta.is_dir() && !entry.file_type().is_ok_and(|t| t.is_symlink()) {
                stack.push(entry.path());
            }
            if let Ok(time) = meta.modified() {
                newest = newest.max(Some(time));
            }
        }
    }
    newest
}

/// Deletes a mission's offline save, so the map starts over.
pub fn reset_save(mission: &Path) -> Result<()> {
    if mission_world(mission).is_none() {
        bail!("{} isn't a mission folder", mission.display());
    }
    let save = mission.join(SAVE);
    if save.exists() {
        std::fs::remove_dir_all(&save)
            .with_context(|| format!("couldn't delete {}", save.display()))?;
    }
    Ok(())
}

/// The user's launch arguments without the ones offline mode already passes (such as
/// `-nosplash` or `-noPause` from the launch options), so none comes twice.
fn not_ours(extra: &[String]) -> Vec<String> {
    let name = |arg: &str| arg.split('=').next().unwrap_or(arg).to_ascii_lowercase();
    extra
        .iter()
        .filter(|a| !GAME_ARGS.iter().any(|ours| name(ours) == name(a)))
        .cloned()
        .collect()
}

/// Arguments that would fight offline mode: it sets the mission and mods itself, and joining a
/// server isn't playing offline.
const CONFLICTS: [&str; 5] = ["-connect", "-password", "-port", "-mod", "-mission"];

/// Extra launch arguments that would fight offline mode.
fn check_extra(extra: &[String]) -> Result<()> {
    let conflicts: Vec<&str> = extra
        .iter()
        .filter(|a| {
            let name = a.split('=').next().unwrap_or(a).to_ascii_lowercase();
            CONFLICTS.contains(&name.as_str())
        })
        .map(String::as_str)
        .collect();
    if !conflicts.is_empty() {
        bail!(
            "remove {} from the extra launch arguments (Settings) to play offline",
            conflicts.join(" ")
        );
    }
    Ok(())
}

/// The record of the `Missions` link this browser made in the game folder (Linux).
#[cfg(not(windows))]
const LINK_FILE: &str = "offline-link.json";

#[cfg(not(windows))]
#[derive(Serialize, Deserialize)]
struct LinkRecord {
    link: PathBuf,
    target: PathBuf,
}

/// Makes `<game>/Missions` a link to `missions` (the browser's copy), so `-mission=Missions/…`
/// finds it. Something else by that name is left alone; an earlier link of ours is replaced.
#[cfg(not(windows))]
pub fn link_missions(game: &Path, missions: &Path, state_dir: &Path) -> Result<()> {
    let link = game.join(MISSIONS);
    let record_path = state_dir.join(LINK_FILE);
    let recorded: Option<LinkRecord> = std::fs::read(&record_path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok());
    if link.symlink_metadata().is_ok() {
        let to = link.read_link().ok();
        if to.as_deref() == Some(missions) {
            if recorded.is_some_and(|r| r.link == link && r.target == missions) {
                return Ok(());
            }
        } else if recorded.is_some_and(|r| r.link == link && to.as_ref() == Some(&r.target)) {
            std::fs::remove_file(&link)
                .with_context(|| format!("couldn't replace {}", link.display()))?;
        } else {
            bail!(
                "{} already exists and isn't the browser's (perhaps Offline Mode installed by \
                 hand); move it elsewhere to play offline from here",
                link.display()
            );
        }
    }
    if link.symlink_metadata().is_err() {
        std::os::unix::fs::symlink(missions, &link)
            .with_context(|| format!("couldn't link {} into the game folder", MISSIONS))?;
    }
    let record = LinkRecord {
        link: link.clone(),
        target: missions.to_path_buf(),
    };
    let written = std::fs::create_dir_all(state_dir)
        .map_err(anyhow::Error::from)
        .and_then(|()| write_atomic(&record_path, &serde_json::to_vec(&record)?));
    if let Err(e) = written {
        // Unrecorded, it would never be cleaned up.
        let _ = std::fs::remove_file(&link);
        return Err(e.context("couldn't record the Missions link"));
    }
    Ok(())
}

/// Removes the `Missions` link made earlier, if it's still the browser's.
#[cfg(not(windows))]
pub fn remove_mission_link(state_dir: &Path) {
    let record_path = state_dir.join(LINK_FILE);
    let Ok(bytes) = std::fs::read(&record_path) else {
        return;
    };
    if let Ok(record) = serde_json::from_slice::<LinkRecord>(&bytes)
        && record.link.read_link().is_ok_and(|to| to == record.target)
    {
        let _ = std::fs::remove_file(&record.link);
    }
    let _ = std::fs::remove_file(record_path);
}

/// The Linux launch through `steam` (the command that runs Steam, with any leading arguments),
/// given the mods' link names.
#[cfg(not(windows))]
fn steam_launch(
    mut steam: Vec<String>,
    folder: &str,
    mod_links: &[String],
    extra: &[String],
) -> Result<Launch> {
    let program = steam.remove(0);
    let mut args = steam;
    args.extend(["-applaunch", "221100", "-nolauncher"].map(String::from));
    args.push(format!("-mission={MISSIONS}/{folder}"));
    args.extend(GAME_ARGS.map(String::from));
    if !mod_links.is_empty() {
        args.push(format!("-mod={}", mod_links.join(";")));
    }
    args.extend(not_ours(extra));
    let length: usize = args.iter().map(|a| a.len() + 1).sum();
    if length > crate::launch::STEAM_ARGS_LIMIT {
        bail!(
            "the launch arguments are {length} characters, more than Steam on Linux handles \
             (about 1,000); shorten the extra launch arguments"
        );
    }
    Ok(Launch {
        program,
        args,
        cwd: None,
    })
}

/// The Windows launch: the game itself (no BattlEye offline), with the mission's full path.
#[cfg_attr(not(windows), allow(dead_code))]
fn windows_launch(game: &Path, mission: &Path, mods: &[PathBuf], extra: &[String]) -> Launch {
    let mut args = vec![format!("-mission={}", mission.display())];
    args.extend(GAME_ARGS.map(String::from));
    if !mods.is_empty() {
        let list: Vec<String> = mods.iter().map(|p| p.display().to_string()).collect();
        args.push(format!("-mod={}", list.join(";")));
    }
    args.extend(not_ours(extra));
    Launch {
        program: game.join("DayZ_x64.exe").display().to_string(),
        args,
        cwd: Some(game.to_path_buf()),
    }
}

/// The launch for a mission, whose mods are downloaded. On Linux this makes the links it needs.
#[cfg(not(windows))]
pub fn build(
    install: &Install,
    mission: &Mission,
    extra: &[String],
    state_dir: &Path,
) -> Result<Launch> {
    check_extra(extra)?;
    let missions = mission
        .path
        .parent()
        .context("the mission folder has no parent")?;
    let steam = crate::steam::steam_command()?;
    let names: Vec<String> = mission.mods.iter().map(|&id| mods::link_name(id)).collect();
    let plan = steam_launch(steam, &mission.folder, &names, extra)?;
    for &id in &mission.mods {
        if !install.workshop.join(id.to_string()).is_dir() {
            bail!("{} needs mod {id}, which isn't downloaded", mission.title);
        }
    }
    // Only now that the launch can go ahead: the links it names.
    link_missions(&install.game, missions, state_dir)?;
    if mission.mods.is_empty() {
        mods::remove_links(install, state_dir);
    } else {
        mods::link_mods(install, &mission.mods, state_dir)?;
    }
    Ok(plan)
}

/// The launch for a mission, whose mods are downloaded.
#[cfg(windows)]
pub fn build(
    install: &Install,
    mission: &Mission,
    extra: &[String],
    _state_dir: &Path,
) -> Result<Launch> {
    check_extra(extra)?;
    let program = install.game.join("DayZ_x64.exe");
    if !program.is_file() {
        bail!("{} is missing", program.display());
    }
    let mods = mods::mod_paths(install, &mission.mods)?;
    Ok(windows_launch(&install.game, &mission.path, &mods, extra))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("dzb-offline-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    fn mission(missions: &Path, name: &str) -> PathBuf {
        let path = missions.join(name);
        std::fs::create_dir_all(&path).unwrap();
        std::fs::write(path.join("init.c"), "void main() {}").unwrap();
        path
    }

    fn release(tag: &str) -> Release {
        Release {
            tag: tag.into(),
            page: String::new(),
            asset_id: 1,
            size: 100,
            updated: "2023-09-01T10:57:07Z".into(),
            url: String::new(),
            sha256: None,
        }
    }

    #[test]
    fn validates_mission_folders() {
        let root = temp("validate");
        let chernarus = mission(&root, "DayZCommunityOfflineMode.ChernarusPlus");
        assert_eq!(mission_world(&chernarus).as_deref(), Some("chernarusplus"));
        let no_init = root.join("Empty.Enoch");
        std::fs::create_dir_all(&no_init).unwrap();
        assert_eq!(mission_world(&no_init), None, "no init.c");
        let no_world = root.join("NoWorld");
        std::fs::create_dir_all(&no_world).unwrap();
        std::fs::write(no_world.join("init.c"), "").unwrap();
        assert_eq!(mission_world(&no_world), None, "no .world");
        assert_eq!(mission_world(&mission(&root, "Odd name.Enoch")), None);
        mission(&root, "DayZCommunityOfflineMode.Namalsk");
        mission(&root, "DayZCommunityOfflineMode.Enoch");
        mission(&root, "DayZCommunityOfflineMode.Sakhal");
        std::fs::create_dir_all(root.join("DayZCommunityOfflineMode.Enoch").join(SAVE)).unwrap();
        let found = missions(&root);
        let titles: Vec<&str> = found.iter().map(|m| m.title.as_str()).collect();
        assert_eq!(titles, ["Chernarus", "Livonia", "Namalsk", "Sakhal"]);
        assert!(found[1].saved && !found[0].saved);
        assert_eq!(found[2].mods, NAMALSK_MODS);
        assert!(found[0].mods.is_empty());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn notices_a_changed_release() {
        let old = release("v4.1");
        assert!(changed(None, &old));
        assert!(!changed(Some(&old), &old.clone()));
        assert!(changed(Some(&old), &release("v4.2")));
        let reuploaded = Release {
            updated: "2024-01-01T00:00:00Z".into(),
            ..old.clone()
        };
        assert!(changed(Some(&old), &reuploaded));
        // The record is read back as written.
        let root = temp("record");
        std::fs::create_dir_all(root.join(MISSIONS)).unwrap();
        assert_eq!(installed(&root), None);
        std::fs::write(root.join(RECORD), serde_json::to_vec(&old).unwrap()).unwrap();
        assert_eq!(installed(&root), Some(old));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn picks_the_download() {
        let release: GithubRelease = serde_json::from_str(
            r#"{"tag_name":"v4.1","html_url":"https://example.com/r","assets":[
                {"id":7,"name":"notes.txt","size":1,"browser_download_url":"x"},
                {"id":124092760,"name":"DayZ.Community.OfflineMode.zip","size":12960332,
                 "updated_at":"2023-09-01T10:57:07Z","browser_download_url":"https://example.com/z",
                 "digest":"sha256:ABCD"}]}"#,
        )
        .unwrap();
        let picked = pick(release).unwrap();
        assert_eq!((picked.asset_id, picked.size), (124092760, 12960332));
        assert_eq!(picked.sha256.as_deref(), Some("abcd"));
    }

    #[test]
    fn updates_keep_the_saves() {
        let root = temp("swap");
        let current = root.join(MISSIONS);
        let chernarus = mission(&current, "COM.ChernarusPlus");
        std::fs::create_dir_all(chernarus.join(SAVE)).unwrap();
        std::fs::write(chernarus.join(SAVE).join("players.db"), "me").unwrap();
        mission(&current, "COM.Dropped");
        let new = root.join("new");
        mission(&new, "COM.ChernarusPlus");
        mission(&new, "COM.Enoch");
        swap_in(&root, &new).unwrap();
        let save = current
            .join("COM.ChernarusPlus")
            .join(SAVE)
            .join("players.db");
        assert_eq!(std::fs::read_to_string(save).unwrap(), "me");
        assert!(current.join("COM.Enoch/init.c").is_file());
        assert!(current.join("COM.Dropped/init.c").is_file(), "kept");
        assert!(!root.join(OLD).exists());
        // An install that stopped after putting the new missions in place, before moving the
        // saves over: they still count.
        std::fs::rename(&current, root.join(OLD)).unwrap();
        mission(&current, "COM.ChernarusPlus");
        mission(&new, "COM.ChernarusPlus");
        swap_in(&root, &new).unwrap();
        assert!(current.join("COM.ChernarusPlus").join(SAVE).is_dir());
        assert!(current.join("COM.Enoch/init.c").is_file());
        assert!(!root.join(OLD).exists());
        // Resetting deletes the save only.
        reset_save(&current.join("COM.ChernarusPlus")).unwrap();
        assert!(!current.join("COM.ChernarusPlus").join(SAVE).exists());
        assert!(current.join("COM.ChernarusPlus/init.c").is_file());
        assert!(reset_save(&root).is_err(), "not a mission");
        std::fs::remove_dir_all(root).unwrap();
    }

    /// Writes a save with one file, last changed `age` seconds ago.
    fn save(mission: &Path, name: &str, text: &str, age: u64) {
        let dir = mission.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        let mut file = std::fs::File::create(dir.join("players.db")).unwrap();
        file.write_all(text.as_bytes()).unwrap();
        let time = std::time::SystemTime::now() - std::time::Duration::from_secs(age);
        file.set_modified(time).unwrap();
    }

    fn read_save(mission: &Path, name: &str) -> String {
        std::fs::read_to_string(mission.join(name).join("players.db")).unwrap()
    }

    #[test]
    fn a_left_over_save_never_replaces_a_newer_one() {
        let root = temp("leftover");
        let (current, old) = (root.join(MISSIONS), root.join(OLD));
        // An update moved the new missions in but couldn't move the saves back; then the user
        // played Chernarus (a new save) but not Livonia.
        let chernarus = mission(&old, "COM.ChernarusPlus");
        save(&chernarus, SAVE, "before the update", 3600);
        let livonia = mission(&old, "COM.Enoch");
        save(&livonia, SAVE, "livonia", 7200);
        let chernarus_now = mission(&current, "COM.ChernarusPlus");
        save(&chernarus_now, SAVE, "played since", 60);
        let livonia_now = mission(&current, "COM.Enoch");
        assert_eq!(waiting_saves(&root), ["COM.ChernarusPlus", "COM.Enoch"]);
        assert!(waiting_saves(&current).is_empty());

        // The next update settles them first: the newer save is played, the older kept beside.
        let new = root.join("new");
        mission(&new, "COM.ChernarusPlus");
        mission(&new, "COM.Enoch");
        swap_in(&root, &new).unwrap();
        assert_eq!(read_save(&chernarus_now, SAVE), "played since");
        assert_eq!(
            read_save(&chernarus_now, "storage_-1.bak"),
            "before the update"
        );
        assert_eq!(read_save(&livonia_now, SAVE), "livonia");
        assert!(!old.exists() && waiting_saves(&root).is_empty());

        // The other way round: the waiting save is the newer one.
        let old_chernarus = mission(&old, "COM.ChernarusPlus");
        save(&old_chernarus, SAVE, "newest", 0);
        // And one set aside before stays set aside, under a free name.
        save(&old_chernarus, "storage_-1.bak", "set aside in .old", 9000);
        assert_eq!(
            restore_saves(&root).unwrap(),
            ["COM.ChernarusPlus".to_string()]
        );
        assert_eq!(read_save(&chernarus_now, SAVE), "newest");
        let mut kept: Vec<String> = ["storage_-1.bak", "storage_-1.bak2", "storage_-1.bak3"]
            .iter()
            .map(|n| read_save(&chernarus_now, n))
            .collect();
        kept.sort();
        assert_eq!(
            kept,
            ["before the update", "played since", "set aside in .old"]
        );
        assert!(!old.exists());
        assert!(restore_saves(&root).unwrap().is_empty(), "nothing waiting");

        // Saves set aside are carried over by an update too.
        mission(&new, "COM.ChernarusPlus");
        swap_in(&root, &new).unwrap();
        let chernarus_now = current.join("COM.ChernarusPlus");
        assert_eq!(read_save(&chernarus_now, SAVE), "newest");
        assert!(chernarus_now.join("storage_-1.bak3").is_dir());
        assert!(current.join("COM.Enoch").join(SAVE).is_dir());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn an_update_keeps_a_shipped_save_aside() {
        let root = temp("shipped");
        let current = root.join(MISSIONS);
        save(&mission(&current, "COM.Enoch"), SAVE, "mine", 3600);
        let new = root.join("new");
        // A release with a save of its own, newer by its files' times: the user's still wins.
        save(&mission(&new, "COM.Enoch"), SAVE, "shipped", 0);
        swap_in(&root, &new).unwrap();
        let enoch = current.join("COM.Enoch");
        assert_eq!(read_save(&enoch, SAVE), "mine");
        assert_eq!(read_save(&enoch, "storage_-1.bak"), "shipped");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn refuses_odd_zips() {
        let options = zip::write::SimpleFileOptions::default();
        type Writer = zip::ZipWriter<std::io::Cursor<Vec<u8>>>;
        let archive = |build: &dyn Fn(&mut Writer)| {
            let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
            build(&mut zip);
            let bytes = zip.finish().unwrap().into_inner();
            zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap()
        };
        let mut fine = archive(&|zip| {
            zip.add_directory("Missions/COM.Enoch", options).unwrap();
            zip.start_file("Missions/COM.Enoch/init.c", options)
                .unwrap();
            zip.write_all(b"void main() {}").unwrap();
        });
        assert!(check_archive(&mut fine).is_ok());
        let mut link = archive(&|zip| {
            zip.add_symlink("Missions", "/etc", options).unwrap();
        });
        assert!(check_archive(&mut link).is_err());
        // Too many entries, or too much once unpacked (though tiny compressed); with small
        // limits, to keep the test quick.
        let mut many = archive(&|zip| {
            for i in 0..=10 {
                zip.add_directory(format!("d{i}"), options).unwrap();
            }
        });
        assert!(check_within(&mut many, 11, 1000).is_ok());
        assert!(check_within(&mut many, 10, 1000).is_err());
        let mut big = archive(&|zip| {
            let deflated = options.compression_method(zip::CompressionMethod::Deflated);
            zip.start_file("big", deflated).unwrap();
            zip.write_all(&[0u8; 100_000]).unwrap();
        });
        assert!(check_within(&mut big, 10, 100_000).is_ok());
        assert!(check_within(&mut big, 10, 99_999).is_err());
    }

    #[test]
    fn installs_from_a_zip() {
        let root = temp("zip");
        let zip_path = root.join("release.zip");
        {
            let file = std::fs::File::create(&zip_path).unwrap();
            let mut zip = zip::ZipWriter::new(file);
            let options = zip::write::SimpleFileOptions::default();
            zip.start_file("Missions/COM.Enoch/init.c", options)
                .unwrap();
            zip.write_all(b"void main() {}").unwrap();
            zip.start_file("README.md", options).unwrap();
            zip.finish().unwrap();
        }
        let dir = root.join("offline");
        std::fs::create_dir_all(&dir).unwrap();
        install(&dir, &zip_path, &release("v4.1")).unwrap();
        assert_eq!(installed_missions(&dir).len(), 1);
        assert_eq!(installed(&dir).map(|r| r.tag).as_deref(), Some("v4.1"));
        assert!(!dir.join(NEW).exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    #[cfg(unix)]
    fn mission_link_is_ours_only() {
        let root = temp("link");
        let (game, state) = (root.join("DayZ"), root.join("state"));
        std::fs::create_dir_all(&game).unwrap();
        let ours = root.join("offline").join(MISSIONS);
        std::fs::create_dir_all(&ours).unwrap();
        link_missions(&game, &ours, &state).unwrap();
        assert_eq!(game.join(MISSIONS).read_link().unwrap(), ours);
        // Again: fine. Moved copy: our old link is replaced.
        link_missions(&game, &ours, &state).unwrap();
        let moved = root.join("elsewhere").join(MISSIONS);
        std::fs::create_dir_all(&moved).unwrap();
        link_missions(&game, &moved, &state).unwrap();
        assert_eq!(game.join(MISSIONS).read_link().unwrap(), moved);
        remove_mission_link(&state);
        assert!(game.join(MISSIONS).symlink_metadata().is_err());
        // A folder of the user's is never touched, nor a link that isn't ours.
        std::fs::create_dir_all(game.join(MISSIONS)).unwrap();
        assert!(link_missions(&game, &ours, &state).is_err());
        assert!(game.join(MISSIONS).is_dir());
        std::fs::remove_dir(game.join(MISSIONS)).unwrap();
        std::os::unix::fs::symlink(&moved, game.join(MISSIONS)).unwrap();
        assert!(link_missions(&game, &ours, &state).is_err());
        remove_mission_link(&state);
        assert_eq!(game.join(MISSIONS).read_link().unwrap(), moved);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    #[cfg(not(windows))]
    fn linux_offline_arguments() {
        let steam = ["flatpak", "run", "com.valvesoftware.Steam"].map(String::from);
        let launch = steam_launch(
            steam.to_vec(),
            "DayZCommunityOfflineMode.Namalsk",
            &[mods::link_name(2289456201), mods::link_name(2289461232)],
            &["-world=empty".into()],
        )
        .unwrap();
        assert_eq!(launch.program, "flatpak");
        assert_eq!(
            launch.args,
            [
                "run",
                "com.valvesoftware.Steam",
                "-applaunch",
                "221100",
                "-nolauncher",
                "-mission=Missions/DayZCommunityOfflineMode.Namalsk",
                "-nosplash",
                "-noPause",
                "-noBenchmark",
                "-filePatching",
                "-doLogs",
                "-scriptDebug=true",
                "-mod=@w11v2zpl;@w11v33lc",
                "-world=empty",
            ]
        );
        let long = vec!["-x".repeat(1000)];
        assert!(steam_launch(vec!["steam".into()], "M.Enoch", &[], &long).is_err());
    }

    #[test]
    fn windows_offline_arguments() {
        let game = Path::new(r"C:\Games\DayZ");
        let mission = Path::new(r"C:\Users\Sam Smith\dayz-browser\offline\Missions\COM.Enoch");
        let launch = windows_launch(game, mission, &[], &[]);
        assert!(launch.program.ends_with("DayZ_x64.exe"));
        assert_eq!(launch.cwd.as_deref(), Some(game));
        assert_eq!(launch.args[0], format!("-mission={}", mission.display()));
        assert_eq!(launch.args[1..], GAME_ARGS);
        let mods = [
            PathBuf::from(r"C:\W\2289456201"),
            PathBuf::from(r"C:\W\2289461232"),
        ];
        let launch = windows_launch(game, mission, &mods, &["-skipIntro".into()]);
        assert_eq!(
            launch.args[launch.args.len() - 2..],
            [
                format!("-mod={};{}", mods[0].display(), mods[1].display()),
                "-skipIntro".to_string()
            ]
        );
    }

    #[test]
    fn refuses_conflicting_extra_arguments() {
        assert!(check_extra(&["-nosplash".into(), "-name=Sam".into()]).is_ok());
        assert!(check_extra(&["-Mission=x".into()]).is_err());
        assert!(check_extra(&["-connect=1.2.3.4".into()]).is_err());
        // Launch options offline mode passes itself come once.
        let extra: Vec<String> = [
            "-name=Sam",
            "-noSplash",
            "-nopause",
            "-skipIntro",
            "-doLogs",
        ]
        .map(String::from)
        .to_vec();
        assert_eq!(not_ours(&extra), ["-name=Sam", "-skipIntro"]);
        let launch = windows_launch(Path::new("C:\\DayZ"), Path::new("C:\\M.Enoch"), &[], &extra);
        let count = |name: &str| {
            launch
                .args
                .iter()
                .filter(|a| a.eq_ignore_ascii_case(name))
                .count()
        };
        assert_eq!((count("-nosplash"), count("-nopause")), (1, 1));
    }
}
