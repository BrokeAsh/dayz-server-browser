//! Updating the browser from its GitHub releases. A check finds a newer release with a download
//! for this system; updating downloads it, checks it against the release's `SHA256SUMS`, puts the
//! new files (browser, Workshop helper, map overlay, Steamworks library) in place of the old ones,
//! and starts the new browser.

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

const LATEST: &str = "https://api.github.com/repos/BrokeAsh/dayz-server-browser/releases/latest";
/// The end of this system's download's name, e.g. `dayz-browser-v0.2.0-linux-x86_64.tar.gz`.
const ASSET_END: &str = if cfg!(windows) {
    "-windows-x86_64.zip"
} else {
    "-linux-x86_64.tar.gz"
};
const EXE: &str = if cfg!(windows) {
    "dayz-browser.exe"
} else {
    "dayz-browser"
};
/// Where a download is unpacked, inside the install folder (so moving files in is a rename).
const STAGING: &str = ".update";
/// What a replaced file is renamed to until it can be deleted (on Windows, a running program
/// can be renamed but not deleted).
const OLD: &str = ".old";

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Held while updating, so the clean-up at start can't delete what an update is using (such as
/// the old files a failed update puts back).
static UPDATING: Mutex<()> = Mutex::new(());

#[derive(Debug, Clone)]
pub struct Release {
    /// Without the `v`: `0.2.0`.
    pub version: String,
    /// The release's page, with what changed.
    pub page: String,
    asset_name: String,
    asset_url: String,
    sums_url: String,
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
    name: String,
    browser_download_url: String,
}

/// Downloads get longer than the shared agent allows for a body. Requests are hours apart, so
/// there's no keeping connections open (a server may have closed one by the next).
fn agent() -> &'static ureq::Agent {
    static AGENT: OnceLock<ureq::Agent> = OnceLock::new();
    AGENT.get_or_init(|| {
        ureq::Agent::config_builder()
            .max_idle_connections(0)
            .max_idle_connections_per_host(0)
            .timeout_connect(Some(Duration::from_secs(15)))
            .timeout_recv_response(Some(Duration::from_secs(30)))
            .timeout_recv_body(Some(Duration::from_secs(30 * 60)))
            .user_agent(crate::http::USER_AGENT)
            .build()
            .new_agent()
    })
}

/// The latest release, if it's newer than this version and has a download for this system.
pub fn check() -> Result<Option<Release>> {
    // For testing against a stand-in release.
    let url = std::env::var("DAYZ_BROWSER_RELEASES_URL").unwrap_or_else(|_| LATEST.into());
    let release: GithubRelease = agent()
        .get(&url)
        .header("Accept", "application/vnd.github+json")
        .call()
        .context("couldn't ask GitHub for the latest version")?
        .body_mut()
        .read_json()
        .context("GitHub's answer about the latest version was unreadable")?;
    let version = release.tag_name.trim_start_matches('v').to_string();
    if !newer(&version, VERSION) {
        return Ok(None);
    }
    let find = |pred: &dyn Fn(&str) -> bool| {
        release
            .assets
            .iter()
            .find(|a| pred(&a.name))
            .map(|a| (a.name.clone(), a.browser_download_url.clone()))
    };
    let Some((asset_name, asset_url)) =
        find(&|n| n.starts_with("dayz-browser-") && n.ends_with(ASSET_END))
    else {
        return Ok(None);
    };
    let Some((_, sums_url)) = find(&|n| n == "SHA256SUMS") else {
        return Ok(None);
    };
    Ok(Some(Release {
        version,
        page: release.html_url,
        asset_name,
        asset_url,
        sums_url,
    }))
}

/// Whether version `a` is newer than `b` (both like `1.2.3`; anything after a `-` is ignored).
fn newer(a: &str, b: &str) -> bool {
    let parts = |v: &str| -> Vec<u64> {
        v.split('-')
            .next()
            .unwrap_or_default()
            .split('.')
            .map(|p| p.parse().unwrap_or(0))
            .collect()
    };
    let (a, b) = (parts(a), parts(b));
    let len = a.len().max(b.len());
    let at = |v: &[u64], i: usize| v.get(i).copied().unwrap_or(0);
    (0..len)
        .map(|i| at(&a, i).cmp(&at(&b, i)))
        .find(|o| o.is_ne())
        .is_some_and(|o| o.is_gt())
}

/// The folder the browser runs from, if it can update it (not one a package manager owns).
pub fn install_dir() -> Result<PathBuf> {
    let exe = std::env::current_exe().context("couldn't tell where the browser is")?;
    let exe = exe.canonicalize().unwrap_or(exe);
    let dir = exe
        .parent()
        .context("couldn't tell where the browser is")?
        .to_path_buf();
    let probe = dir.join(".update-probe");
    std::fs::File::create(&probe).with_context(|| {
        format!(
            "the browser can't change its own folder ({}); update it the way you installed it",
            dir.display()
        )
    })?;
    let _ = std::fs::remove_file(probe);
    Ok(dir)
}

/// Updates to `release`: downloads it (reporting bytes so far, and the total if known), and once
/// it's checked, calls `before_install` and puts the new files in place. Returns the new browser
/// program. Nothing changes if the download fails.
pub fn update(
    release: &Release,
    progress: impl FnMut(u64, Option<u64>),
    before_install: impl FnOnce(),
) -> Result<PathBuf> {
    let _updating = UPDATING.lock().unwrap_or_else(|e| e.into_inner());
    let dir = install_dir()?;
    let staging = dir.join(STAGING);
    let result = download(release, &staging, progress).and_then(|files| {
        before_install();
        swap_in(&dir, &files)
    });
    let _ = std::fs::remove_dir_all(&staging);
    result.map(|()| dir.join(EXE))
}

/// Downloads and checks a release, and unpacks it into `staging`. Returns the folder holding the
/// new files.
fn download(
    release: &Release,
    staging: &Path,
    mut progress: impl FnMut(u64, Option<u64>),
) -> Result<PathBuf> {
    let _ = std::fs::remove_dir_all(staging);
    std::fs::create_dir_all(staging)
        .with_context(|| format!("couldn't create {}", staging.display()))?;

    let sums = agent()
        .get(&release.sums_url)
        .call()
        .and_then(|mut r| r.body_mut().read_to_string())
        .context("couldn't download the release's checksums")?;
    let expected = sums
        .lines()
        .filter_map(|line| line.split_once(char::is_whitespace))
        .find(|(_, name)| name.trim().trim_start_matches('*') == release.asset_name)
        .map(|(sum, _)| sum.to_lowercase())
        .context("the release's checksums don't list this download")?;

    let archive = staging.join(&release.asset_name);
    let mut response = agent()
        .get(&release.asset_url)
        .call()
        .context("couldn't download the update")?;
    let total = response.body().content_length();
    let mut body = response.body_mut().as_reader();
    let mut file = std::fs::File::create(&archive)
        .with_context(|| format!("couldn't write {}", archive.display()))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    let mut done = 0u64;
    loop {
        let n = body.read(&mut buf).context("the download broke off")?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        file.write_all(&buf[..n])
            .with_context(|| format!("couldn't write {}", archive.display()))?;
        done += n as u64;
        progress(done, total);
    }
    file.sync_all()?;
    drop(file);
    let actual: String = hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    if actual != expected {
        bail!("the download doesn't match the release's checksum; nothing was changed");
    }

    let unpacked = staging.join("files");
    unpack(&archive, &unpacked).context("couldn't unpack the update")?;
    // Downloads hold one folder named after the release.
    let mut folders = std::fs::read_dir(&unpacked)?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_dir());
    let folder = folders
        .next()
        .filter(|f| f.join(EXE).is_file())
        .context("the download doesn't hold the browser")?;
    Ok(folder)
}

#[cfg(not(windows))]
fn unpack(archive: &Path, to: &Path) -> Result<()> {
    let file = std::fs::File::open(archive)?;
    // `unpack` keeps every entry inside `to`, and keeps the programs' permissions.
    tar::Archive::new(flate2::read::GzDecoder::new(file)).unpack(to)?;
    Ok(())
}

#[cfg(windows)]
fn unpack(archive: &Path, to: &Path) -> Result<()> {
    let file = std::fs::File::open(archive)?;
    // `extract` keeps every entry inside `to`.
    zip::ZipArchive::new(file)?.extract(to)?;
    Ok(())
}

/// Moves every file in `new_files` into `dir`, replacing what's there, and undoes it all if one
/// can't be moved.
fn swap_in(dir: &Path, new_files: &Path) -> Result<()> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(new_files)?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_file())
        .collect();
    // The browser last, so a failure leaves the running one where it was as long as possible.
    files.sort_by_key(|p| p.file_name().is_some_and(|n| n == EXE));
    let mut moved: Vec<(PathBuf, Option<PathBuf>)> = Vec::new();
    let result = files.iter().try_for_each(|new| {
        let name = new.file_name().context("a file without a name")?;
        let target = dir.join(name);
        let old = if target.exists() {
            let old = dir.join(format!("{}{OLD}", name.to_string_lossy()));
            let _ = std::fs::remove_file(&old);
            std::fs::rename(&target, &old)
                .with_context(|| format!("couldn't move {} aside", target.display()))?;
            Some(old)
        } else {
            None
        };
        moved.push((target.clone(), old));
        std::fs::rename(new, &target)
            .with_context(|| format!("couldn't put the new {} in place", target.display()))
    });
    if let Err(e) = result {
        for (target, old) in moved.into_iter().rev() {
            let _ = std::fs::remove_file(&target);
            if let Some(old) = old {
                let _ = std::fs::rename(&old, &target);
            }
        }
        return Err(e);
    }
    for (_, old) in moved {
        if let Some(old) = old {
            // Windows keeps a running program's file; the next start deletes it.
            let _ = std::fs::remove_file(old);
        }
    }
    Ok(())
}

/// Deletes what an update left behind: replaced files Windows wouldn't delete while they ran,
/// and an unfinished download.
pub fn clean_up() {
    let _updating = UPDATING.lock().unwrap_or_else(|e| e.into_inner());
    let Ok(dir) = install_dir() else {
        return;
    };
    let _ = std::fs::remove_dir_all(dir.join(STAGING));
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return;
    };
    for entry in entries.flatten() {
        if entry.file_name().to_string_lossy().ends_with(OLD) {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// Starts the new browser on its own, so it outlives this one. It logs where this one did.
pub fn start(program: &Path) -> Result<()> {
    let mut command = Command::new(program);
    command.stdin(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        command.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
    }
    let mut child = command
        .spawn()
        .with_context(|| format!("couldn't start {}", program.display()))?;
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compares_versions() {
        assert!(newer("0.2.0", "0.1.0"));
        assert!(newer("0.10.0", "0.9.3"));
        assert!(newer("1.0", "0.9.9"));
        assert!(newer("0.1.1", "0.1"));
        assert!(!newer("0.1.0", "0.1.0"));
        assert!(!newer("0.1.0", "0.2.0"));
        assert!(!newer("0.2.0-rc1", "0.2.0"));
    }

    #[test]
    fn installs_and_rolls_back() {
        let root = std::env::temp_dir().join(format!("dzb-update-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (dir, new) = (root.join("app"), root.join("new"));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::create_dir_all(&new).unwrap();
        std::fs::write(dir.join("a"), "old a").unwrap();
        std::fs::write(new.join("a"), "new a").unwrap();
        std::fs::write(new.join("b"), "new b").unwrap();
        swap_in(&dir, &new).unwrap();
        assert_eq!(std::fs::read_to_string(dir.join("a")).unwrap(), "new a");
        assert_eq!(std::fs::read_to_string(dir.join("b")).unwrap(), "new b");
        assert!(!dir.join("a.old").exists());

        // A file that can't be moved in (a folder is in its way) undoes the rest.
        std::fs::write(new.join("a"), "newer a").unwrap();
        std::fs::write(new.join("c"), "new c").unwrap();
        std::fs::create_dir_all(dir.join("c.old/x")).unwrap();
        std::fs::create_dir_all(dir.join("c/x")).unwrap();
        assert!(swap_in(&dir, &new).is_err());
        assert_eq!(std::fs::read_to_string(dir.join("a")).unwrap(), "new a");
        std::fs::remove_dir_all(&root).unwrap();
    }
}
