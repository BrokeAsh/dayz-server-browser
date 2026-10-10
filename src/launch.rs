//! Starting DayZ connected to a server with its mods.
//!
//! - Linux: through Steam (`steam -applaunch 221100 -nolauncher ...`), which runs the game under
//!   Proton with BattlEye, as the other Linux launchers do. Steam hangs when these arguments get
//!   past about 1,000 characters, so mods are passed as short links in the game folder.
//! - Windows: `DayZ_BE.exe` directly with full mod paths, as the official launcher and DZSA do.
//!
//! `-connect` carries the query port too (`ip:port:queryport`, as the official launcher writes
//! it), so the map overlay can ask the server for its name.

use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::mods;
use crate::server::Server;
use crate::steam::Install;

/// Arguments the browser sets itself; extra arguments mustn't repeat them.
pub const MANAGED: &[&str] = &["-connect", "-mod", "-password", "-port"];

/// Steam on Linux hangs past roughly 1,000 characters of launch arguments.
#[cfg(not(windows))]
const STEAM_ARGS_LIMIT: usize = 950;

#[derive(Debug, Clone)]
pub struct Launch {
    pub program: String,
    pub args: Vec<String>,
    pub cwd: Option<PathBuf>,
}

impl Launch {
    /// The command line with the password hidden, for the log.
    pub fn display(&self) -> String {
        let args: Vec<String> = self
            .args
            .iter()
            .map(|a| {
                if a.to_ascii_lowercase().starts_with("-password=") {
                    "-password=<hidden>".into()
                } else {
                    a.clone()
                }
            })
            .collect();
        format!("{} {}", self.program, args.join(" "))
    }

    pub fn spawn(&self) -> Result<()> {
        let mut command = Command::new(&self.program);
        command
            .args(&self.args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        if let Some(dir) = &self.cwd {
            command.current_dir(dir);
        }
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }
        let mut child = command
            .spawn()
            .with_context(|| format!("couldn't start {}", self.program))?;
        // Steam hands the launch to its running instance and exits; reap it in the background.
        std::thread::spawn(move || {
            let _ = child.wait();
        });
        Ok(())
    }
}

/// Splits the extra-arguments setting like a command line (quotes allowed).
pub fn parse_extra(text: &str) -> Result<Vec<String>> {
    shlex::split(text).context("the extra launch arguments have an unclosed quote")
}

/// Extra arguments that the browser already sets.
pub fn managed_conflicts(args: &[String]) -> Vec<String> {
    args.iter()
        .filter(|a| {
            let name = a.split('=').next().unwrap_or(a).to_ascii_lowercase();
            MANAGED.contains(&name.as_str())
        })
        .cloned()
        .collect()
}

fn connect_arg(server: &Server) -> String {
    format!(
        "-connect={}:{}:{}",
        server.ip, server.game_port, server.query_port
    )
}

/// The launch for a server with these mods (in load order), already downloaded.
#[cfg(not(windows))]
pub fn build(
    install: &Install,
    server: &Server,
    mod_ids: &[u64],
    password: Option<&str>,
    extra: &[String],
    state_dir: &Path,
) -> Result<Launch> {
    let steam = crate::steam::steam_command()?;
    build_steam(steam, install, server, mod_ids, password, extra, state_dir)
}

/// The Linux launch through `steam` (the command that runs Steam, with any leading arguments).
#[cfg(not(windows))]
fn build_steam(
    mut steam: Vec<String>,
    install: &Install,
    server: &Server,
    mod_ids: &[u64],
    password: Option<&str>,
    extra: &[String],
    state_dir: &Path,
) -> Result<Launch> {
    let program = steam.remove(0);
    let mut args = steam;
    args.extend(["-applaunch", "221100", "-nolauncher"].map(String::from));
    args.push(connect_arg(server));
    if let Some(password) = password.filter(|p| !p.is_empty()) {
        args.push(format!("-password={password}"));
    }
    if !mod_ids.is_empty() {
        let names: Vec<String> = mod_ids.iter().map(|&id| mods::link_name(id)).collect();
        args.push(format!("-mod={}", names.join(";")));
    }
    args.extend(extra.iter().cloned());
    let length: usize = args.iter().map(|a| a.len() + 1).sum();
    if length > STEAM_ARGS_LIMIT {
        bail!(
            "the launch arguments are {length} characters, more than Steam on Linux handles \
             (about 1,000); shorten the extra launch arguments"
        );
    }
    // Only now that the launch can go ahead: the links those names point at.
    if mod_ids.is_empty() {
        mods::remove_links(install, state_dir);
    } else {
        mods::link_mods(install, mod_ids, state_dir)?;
    }
    Ok(Launch {
        program,
        args,
        cwd: None,
    })
}

/// The launch for a server with these mods (in load order), already downloaded.
#[cfg(windows)]
pub fn build(
    install: &Install,
    server: &Server,
    mod_ids: &[u64],
    password: Option<&str>,
    extra: &[String],
    _state_dir: &Path,
) -> Result<Launch> {
    let program = install.game.join("DayZ_BE.exe");
    if !program.is_file() {
        bail!("{} is missing", program.display());
    }
    let mut args: Vec<String> = ["0", "1", "1", "-exe", "DayZ_x64.exe"]
        .map(String::from)
        .to_vec();
    if !mod_ids.is_empty() {
        let paths = mods::mod_paths(install, mod_ids)?;
        let list: Vec<String> = paths.iter().map(|p| p.display().to_string()).collect();
        args.push(format!("-mod={}", list.join(";")));
    }
    args.push(connect_arg(server));
    if let Some(password) = password.filter(|p| !p.is_empty()) {
        args.push(format!("-password={password}"));
    }
    args.extend(extra.iter().cloned());
    Ok(Launch {
        program: program.display().to_string(),
        args,
        cwd: Some(install.game.clone()),
    })
}

/// [`game_running`], asked at most every couple of seconds: for checks made every frame (looking
/// through the process list each time would be slow).
pub fn game_running_lately() -> bool {
    static LAST: Mutex<Option<(Instant, bool)>> = Mutex::new(None);
    let mut last = LAST.lock().unwrap_or_else(|e| e.into_inner());
    match *last {
        Some((at, running)) if at.elapsed() < Duration::from_secs(2) => running,
        _ => {
            let running = game_running();
            *last = Some((Instant::now(), running));
            running
        }
    }
}

/// Whether DayZ is already running.
#[cfg(target_os = "linux")]
pub fn game_running() -> bool {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return false;
    };
    entries.flatten().any(|e| {
        std::fs::read_to_string(e.path().join("comm"))
            .is_ok_and(|comm| comm.trim_end().eq_ignore_ascii_case("DayZ_x64.exe"))
    })
}

#[cfg(windows)]
pub fn game_running() -> bool {
    crate::win::process_running("DayZ_x64.exe")
}

#[cfg(not(any(windows, target_os = "linux")))]
pub fn game_running() -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extra_arguments() {
        let args = parse_extra(r#"-nosplash -name="Survivor One" -skipIntro"#).unwrap();
        assert_eq!(args, ["-nosplash", "-name=Survivor One", "-skipIntro"]);
        assert!(parse_extra(r#"-name="open"#).is_err());
        let conflicts = managed_conflicts(&["-nosplash".into(), "-Connect=1.2.3.4".into()]);
        assert_eq!(conflicts, ["-Connect=1.2.3.4"]);
    }

    #[test]
    #[cfg(unix)]
    fn linux_launch_through_steam() {
        let root = std::env::temp_dir().join(format!("dzb-launch-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let install = Install {
            game: root.join("steamapps/common/DayZ"),
            workshop: root.join("steamapps/workshop/content/221100"),
            manifest: root.join("steamapps/workshop/appworkshop_221100.acf"),
            how: "test",
        };
        std::fs::create_dir_all(&install.game).unwrap();
        std::fs::create_dir_all(install.workshop.join("1559212036")).unwrap();
        let server = Server {
            ip: "203.0.113.7".into(),
            game_port: 2302,
            query_port: 27016,
            ..Default::default()
        };
        let state = root.join("state");
        let flatpak = ["flatpak", "run", "com.valvesoftware.Steam"]
            .map(String::from)
            .to_vec();
        let launch = build_steam(
            flatpak,
            &install,
            &server,
            &[1559212036],
            Some("pw"),
            &["-nosplash".into()],
            &state,
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
                "-connect=203.0.113.7:2302:27016",
                "-password=pw",
                "-mod=@wpsbbpg",
                "-nosplash",
            ]
        );
        assert!(install.game.join("@wpsbbpg").exists());
        // Too long for Steam: refused.
        let long = vec!["-x".repeat(600), "-y".repeat(600)];
        assert!(
            build_steam(
                vec!["steam".into()],
                &install,
                &server,
                &[],
                None,
                &long,
                &state
            )
            .is_err()
        );
        assert!(
            install.game.join("@wpsbbpg").exists(),
            "a refused launch leaves the links alone"
        );
        build_steam(
            vec!["steam".into()],
            &install,
            &server,
            &[],
            None,
            &[],
            &state,
        )
        .unwrap();
        assert!(
            install.game.join("@wpsbbpg").symlink_metadata().is_err(),
            "no mods: links removed"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn hides_the_password() {
        let launch = Launch {
            program: "steam".into(),
            args: vec![
                "-connect=1.2.3.4:2302:27016".into(),
                "-password=hunter2".into(),
            ],
            cwd: None,
        };
        assert!(!launch.display().contains("hunter2"));
    }
}
