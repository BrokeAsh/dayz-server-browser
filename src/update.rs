//! Updating the browser from its GitHub releases. A check finds a newer release with a download
//! for this system; updating downloads it, checks it against the release's `SHA256SUMS`, makes
//! sure the new browser runs, puts the new files (browser, Workshop helper, map overlay,
//! Steamworks library) in place of the old ones, and starts the new browser.

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::cmp::Ordering;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

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
/// The files a download holds (as the release workflow packs them): the only ones the clean-up
/// after an update touches.
const SHIPPED: &[&str] = if cfg!(windows) {
    &[
        "dayz-browser.exe",
        "dayz-browser-workshop.exe",
        "dayz-map.exe",
        "steam_api64.dll",
        "README.md",
        "CHANGELOG.md",
        "LICENSE-MIT",
        "LICENSE-APACHE",
    ]
} else {
    &[
        "dayz-browser",
        "dayz-browser-workshop",
        "dayz-map",
        "libsteam_api.so",
        "README.md",
        "CHANGELOG.md",
        "LICENSE-MIT",
        "LICENSE-APACHE",
    ]
};
/// Where a download is unpacked, inside the install folder (so moving files in is a rename).
const STAGING: &str = ".update";
/// Locked while updating or cleaning up, by whichever copy of the browser does it, so the
/// clean-up at start can't delete what an update is using (such as the old files a failed update
/// puts back). It stays in the folder: deleting a lock file would let two copies lock two files.
const LOCK: &str = ".update.lock";
/// What a replaced file is renamed to until it can be deleted (on Windows, a running program
/// can be renamed but not deleted).
const OLD: &str = ".old";
/// How long the new browser gets to say its version before it counts as broken.
const VERSION_TIMEOUT: Duration = Duration::from_secs(15);

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

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
pub(crate) fn agent() -> &'static ureq::Agent {
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
    // A testing hook: points the check at a stand-in for GitHub's answer (such as JSON from a
    // local web server), to try updating without publishing a release.
    let url = std::env::var("DAYZ_BROWSER_RELEASES_URL").unwrap_or_else(|_| LATEST.into());
    let mut response = match agent()
        .get(&url)
        .header("Accept", "application/vnd.github+json")
        .call()
    {
        // No release published yet.
        Err(ureq::Error::StatusCode(404)) => return Ok(None),
        result => result.context("couldn't ask GitHub for the latest version")?,
    };
    let release: GithubRelease = response
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

/// Whether version `a` is newer than `b` (both like `1.2.3`, maybe with a pre-release after a
/// `-`, as in `1.2.3-rc1`). As in semver, a pre-release comes before its release.
fn newer(a: &str, b: &str) -> bool {
    compare_versions(a, b).is_gt()
}

fn compare_versions(a: &str, b: &str) -> Ordering {
    let split = |v: &str| -> (Vec<u64>, Option<String>) {
        let v = v.trim();
        let (release, pre) = match v.split_once('-') {
            Some((release, pre)) => (release, Some(pre.to_string())),
            None => (v, None),
        };
        let numbers = release.split('.').map(|p| p.parse().unwrap_or(0)).collect();
        (numbers, pre)
    };
    let ((a, a_pre), (b, b_pre)) = (split(a), split(b));
    let len = a.len().max(b.len());
    let at = |v: &[u64], i: usize| v.get(i).copied().unwrap_or(0);
    (0..len)
        .map(|i| at(&a, i).cmp(&at(&b, i)))
        .find(|o| o.is_ne())
        .unwrap_or_else(|| match (a_pre, b_pre) {
            (None, None) => Ordering::Equal,
            (None, Some(_)) => Ordering::Greater,
            (Some(_), None) => Ordering::Less,
            (Some(a), Some(b)) => compare_pre(&a, &b),
        })
}

/// Pre-release tags, part by part (split at `.`): numbers by value and before words, words
/// alphabetically, and a tag that runs out first comes first (`rc` < `rc.1` < `rc.2` < `rc.10`).
fn compare_pre(a: &str, b: &str) -> Ordering {
    let (mut a, mut b) = (a.split('.'), b.split('.'));
    loop {
        let order = match (a.next(), b.next()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(x), Some(y)) => match (x.parse::<u64>(), y.parse::<u64>()) {
                (Ok(x), Ok(y)) => x.cmp(&y),
                (Ok(_), Err(_)) => Ordering::Less,
                (Err(_), Ok(_)) => Ordering::Greater,
                (Err(_), Err(_)) => x.cmp(y),
            },
        };
        if order.is_ne() {
            return order;
        }
    }
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
/// it's checked and the new browser runs, calls `before_install` and puts the new files in place.
/// Returns the new browser program. Nothing changes if the download fails, the new browser
/// doesn't say it's `release`'s version, or `before_install` says no.
pub fn update(
    release: &Release,
    progress: impl FnMut(u64, Option<u64>),
    before_install: impl FnOnce() -> Result<()>,
) -> Result<PathBuf> {
    let dir = install_dir()?;
    let _lock = lock(&dir)?.context("the browser is already updating (in another window?)")?;
    let staging = dir.join(STAGING);
    let result = download(release, &staging, progress).and_then(|files| {
        check_runs(&files.join(EXE), &release.version)?;
        before_install()?;
        swap_in(&dir, &files)
    });
    let _ = std::fs::remove_dir_all(&staging);
    result.map(|()| dir.join(EXE))
}

/// Takes the update lock in `dir`; `None` if another update or clean-up holds it. It's let go
/// when the file is dropped (or the process ends).
fn lock(dir: &Path) -> Result<Option<std::fs::File>> {
    let path = dir.join(LOCK);
    let file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&path)
        .with_context(|| format!("couldn't open {}", path.display()))?;
    match file.try_lock() {
        Ok(()) => Ok(Some(file)),
        Err(std::fs::TryLockError::WouldBlock) => Ok(None),
        Err(std::fs::TryLockError::Error(e)) => {
            Err(e).with_context(|| format!("couldn't lock {}", path.display()))
        }
    }
}

/// Runs the downloaded browser with `--version` (no window, and not for long), and makes sure it
/// starts on this system and says it's `version`.
fn check_runs(program: &Path, version: &str) -> Result<()> {
    let broken = |why: String| {
        anyhow::anyhow!("the new version doesn't run here ({why}); nothing was changed")
    };
    let mut command = Command::new(program);
    command
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(crate::win::CREATE_NO_WINDOW);
    }
    let mut child = command
        .spawn()
        .map_err(|e| broken(format!("it couldn't start: {e}")))?;
    let deadline = Instant::now() + VERSION_TIMEOUT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(50)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(broken("it didn't say its version in time".into()));
            }
            Err(e) => return Err(broken(format!("couldn't wait for it: {e}"))),
        }
    };
    // A line of text fits in the pipe, so reading only after it exits can't hold it up.
    let mut said = String::new();
    if let Some(mut stdout) = child.stdout.take() {
        let _ = stdout.read_to_string(&mut said);
    }
    let said = said.trim();
    let expected = format!("dayz-browser {version}");
    if !status.success() {
        return Err(broken(format!("it stopped with {status}")));
    }
    if said != expected {
        return Err(broken(format!("it says {said:?} instead of {expected:?}")));
    }
    Ok(())
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
/// can't be moved. If undoing fails too, the error says which files are left as `.old` copies.
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
        let stuck = roll_back(moved);
        if stuck.is_empty() {
            return Err(e);
        }
        return Err(e.context(format!(
            "the update failed, and undoing it went wrong too: {}",
            stuck.join("; ")
        )));
    }
    for (_, old) in moved {
        if let Some(old) = old {
            // Windows keeps a running program's file; the next start deletes it.
            let _ = std::fs::remove_file(old);
        }
    }
    Ok(())
}

/// Puts back the files `swap_in` moved aside, last first. Returns what couldn't be put back.
fn roll_back(moved: Vec<(PathBuf, Option<PathBuf>)>) -> Vec<String> {
    let mut stuck = Vec::new();
    for (target, old) in moved.into_iter().rev() {
        let removed = match std::fs::remove_file(&target) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        };
        let restored = match &old {
            Some(old) => removed.and_then(|()| std::fs::rename(old, &target)),
            None => removed,
        };
        if let Err(why) = restored {
            stuck.push(match old {
                Some(old) => format!(
                    "couldn't put back {} ({why}); the old one is {}",
                    target.display(),
                    old.display()
                ),
                None => format!("couldn't remove the new {} ({why})", target.display()),
            });
        }
    }
    stuck
}

/// Deletes what an update left behind: replaced files Windows wouldn't delete while they ran,
/// and an unfinished download. Skipped while another copy of the browser is updating.
pub fn clean_up() {
    let Ok(dir) = install_dir() else {
        return;
    };
    match lock(&dir) {
        Ok(Some(_lock)) => clean_up_in(&dir),
        Ok(None) => log::info!("another copy of the browser is updating; not cleaning up"),
        Err(e) => log::warn!("couldn't clean up after an update: {e:#}"),
    }
}

/// Deletes the staging folder and the `.old` copies of the shipped files. An `.old` copy whose
/// file is missing (an update that failed and couldn't undo itself) is put back instead.
fn clean_up_in(dir: &Path) {
    let _ = std::fs::remove_dir_all(dir.join(STAGING));
    for name in SHIPPED {
        let (file, old) = (dir.join(name), dir.join(format!("{name}{OLD}")));
        if !old.is_file() {
            continue;
        }
        let result = if file.symlink_metadata().is_ok() {
            std::fs::remove_file(&old)
        } else {
            log::warn!("{} was missing; putting the old one back", file.display());
            std::fs::rename(&old, &file)
        };
        if let Err(e) = result {
            log::debug!("couldn't clean up {}: {e}", old.display());
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
    fn pre_releases_come_before_their_release() {
        assert!(newer("0.2.0", "0.2.0-rc1"));
        assert!(newer("0.2.0-rc1", "0.1.9"));
        assert!(newer("0.2.1-rc1", "0.2.0"));
        assert!(newer("0.2.0-rc2", "0.2.0-rc1"));
        assert!(newer("0.2.0-rc.10", "0.2.0-rc.2"));
        assert!(newer("0.2.0-rc.1", "0.2.0-rc"));
        assert!(newer("0.2.0-rc.1", "0.2.0-beta.3"));
        assert!(newer("0.2.0-alpha", "0.2.0-1"), "words after numbers");
        assert!(!newer("0.2.0-rc1", "0.2.0-rc1"));
        assert!(!newer("0.1.0", "0.2.0-rc1"));
    }

    #[test]
    fn checks_the_new_version_runs() {
        let missing = check_runs(Path::new("/nonexistent/dayz-browser"), "0.2.0").unwrap_err();
        assert!(format!("{missing:#}").contains("nothing was changed"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let dir = std::env::temp_dir().join(format!("dzb-runs-test-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            let script = |name: &str, text: &str| {
                let program = dir.join(name);
                std::fs::write(&program, text).unwrap();
                std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
                program
            };
            let good = script(
                "good",
                "#!/bin/sh\n[ \"$1\" = --version ] && echo 'dayz-browser 0.2.0'\n",
            );
            let failing = script("failing", "#!/bin/sh\necho 'dayz-browser 0.2.0'\nexit 3\n");
            // Another test thread starting a program just as a script was written can briefly
            // keep it open ("text file busy"); try again then.
            let run = |program: &Path, version: &str| {
                for _ in 0..20 {
                    match check_runs(program, version) {
                        Err(e) if format!("{e:#}").contains("busy") => {
                            std::thread::sleep(Duration::from_millis(50));
                        }
                        result => return result,
                    }
                }
                check_runs(program, version)
            };
            run(&good, "0.2.0").unwrap();
            assert!(run(&good, "0.3.0").is_err(), "another version");
            assert!(run(&failing, "0.2.0").is_err(), "a failing program");
            std::fs::remove_dir_all(&dir).unwrap();
        }
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

    #[test]
    fn reports_what_rolling_back_left() {
        let dir = std::env::temp_dir().join(format!("dzb-rollback-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // The old copy went missing, so it can't be put back.
        let stuck = roll_back(vec![(dir.join("a"), Some(dir.join("a.old")))]);
        assert_eq!(stuck.len(), 1);
        assert!(stuck[0].contains("a.old"), "{stuck:?}");
        std::fs::write(dir.join("b.old"), "old b").unwrap();
        std::fs::write(dir.join("b"), "new b").unwrap();
        assert!(roll_back(vec![(dir.join("b"), Some(dir.join("b.old")))]).is_empty());
        assert_eq!(std::fs::read_to_string(dir.join("b")).unwrap(), "old b");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn cleans_up_only_what_it_shipped() {
        let dir = std::env::temp_dir().join(format!("dzb-clean-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(STAGING).join("files")).unwrap();
        let (exe, readme) = (EXE, "README.md");
        assert!(SHIPPED.contains(&exe) && SHIPPED.contains(&readme));
        std::fs::write(dir.join(exe), "new").unwrap();
        std::fs::write(dir.join(format!("{exe}{OLD}")), "old").unwrap();
        // An update that couldn't undo itself left only the old copy.
        std::fs::write(dir.join(format!("{readme}{OLD}")), "old readme").unwrap();
        // Not ours.
        std::fs::write(dir.join(format!("notes.txt{OLD}")), "keep").unwrap();
        std::fs::create_dir_all(dir.join("other.update")).unwrap();

        let held = lock(&dir).unwrap().unwrap();
        assert!(lock(&dir).unwrap().is_none(), "the lock is taken");
        drop(held);
        let _lock = lock(&dir).unwrap().unwrap();
        clean_up_in(&dir);
        assert!(!dir.join(STAGING).exists());
        assert_eq!(std::fs::read_to_string(dir.join(exe)).unwrap(), "new");
        assert!(!dir.join(format!("{exe}{OLD}")).exists());
        assert_eq!(
            std::fs::read_to_string(dir.join(readme)).unwrap(),
            "old readme"
        );
        assert!(!dir.join(format!("{readme}{OLD}")).exists());
        assert!(dir.join(format!("notes.txt{OLD}")).exists());
        assert!(dir.join("other.update").exists());
        drop(_lock);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
