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
use crate::settings::Settings;
use crate::steam::Install;

/// Arguments the browser sets itself or has a setting for (in lower case, without any `=...`);
/// extra arguments mustn't repeat them.
pub const MANAGED: &[&str] = &[
    "-connect",
    "-mod",
    "-password",
    "-port",
    "-name",
    "-nosplash",
    "-skipintro",
    "-nopause",
    "-window",
    "-world",
    "-cpucount",
];

/// Characters left out of the in-game name: quotes, backslashes, and the characters a shell
/// treats specially, any of which could split or break the command line on the way to the game.
const NAME_UNSAFE: &[char] = &['"', '\'', '\\', '`', '$', ';', '&', '|', '<', '>'];

/// Whether a character may be in the in-game name: not a control character or [`NAME_UNSAFE`].
pub fn name_char_allowed(c: char) -> bool {
    !c.is_control() && !NAME_UNSAFE.contains(&c)
}

/// The in-game name as it's passed: trimmed, with only the characters allowed.
pub fn clean_name(name: &str) -> String {
    name.chars()
        .filter(|&c| name_char_allowed(c))
        .collect::<String>()
        .trim()
        .to_string()
}

/// The arguments for the launch settings (name, intro, background, window, menu, CPU cores).
pub fn option_args(settings: &Settings) -> Vec<String> {
    let mut args = Vec::new();
    let name = clean_name(&settings.name);
    if !name.is_empty() {
        args.push(format!("-name={name}"));
    }
    if settings.skip_intro {
        args.extend(["-nosplash".into(), "-skipIntro".into()]);
    }
    if settings.no_pause {
        args.push("-noPause".into());
    }
    if settings.windowed {
        args.push("-window".into());
    }
    if settings.empty_world {
        args.push("-world=empty".into());
    }
    if let Some(cores) = settings.cpu_count.filter(|&n| n > 0) {
        args.push(format!("-cpuCount={cores}"));
    }
    args
}

/// Everything the settings add to a launch: the launch options, then the extra arguments.
pub fn added_args(settings: &Settings) -> Result<Vec<String>> {
    let mut args = option_args(settings);
    args.extend(parse_extra(&settings.extra_args)?);
    Ok(args)
}

/// Arguments as they'd be typed, for showing: a value with spaces in quotes
/// (`-name="Survivor One"`).
pub fn shown_args(args: &[String]) -> String {
    args.iter()
        .map(|a| match a.split_once('=') {
            Some((name, value)) if value.contains(char::is_whitespace) => {
                format!("{name}=\"{value}\"")
            }
            _ if a.contains(char::is_whitespace) => format!("\"{a}\""),
            _ => a.clone(),
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Steam on Linux hangs past roughly 1,000 characters of launch arguments.
#[cfg(not(windows))]
pub(crate) const STEAM_ARGS_LIMIT: usize = 950;

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
        format!("{} {}", self.program, shown_args(&args))
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

/// The launch for a server with these mods (in load order), already downloaded; `extra` is what
/// the settings add ([`added_args`]).
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
             (about 1,000); shorten the extra launch arguments or the name"
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

/// The launch for a server with these mods (in load order), already downloaded; `extra` is what
/// the settings add ([`added_args`]).
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
        let conflicts = managed_conflicts(&["-exThreads=7".into(), "-Connect=1.2.3.4".into()]);
        assert_eq!(conflicts, ["-Connect=1.2.3.4"]);
    }

    #[test]
    fn options_that_duplicate_settings_conflict() {
        let args = parse_extra(
            r#"-nosplash -skipIntro -NAME="Someone Else" -World=empty -cpuCount=4 -noPause -window -profiles=x -nameless"#,
        )
        .unwrap();
        assert_eq!(
            managed_conflicts(&args),
            [
                "-nosplash",
                "-skipIntro",
                "-NAME=Someone Else",
                "-World=empty",
                "-cpuCount=4",
                "-noPause",
                "-window",
            ]
        );
        // Every setting's own arguments count as managed.
        let all = Settings {
            name: "Survivor".into(),
            skip_intro: true,
            no_pause: true,
            windowed: true,
            empty_world: true,
            cpu_count: Some(6),
            ..Default::default()
        };
        let own = option_args(&all);
        assert_eq!(managed_conflicts(&own), own);
    }

    #[test]
    fn launch_options() {
        assert!(option_args(&Settings::default()).is_empty());
        let all = Settings {
            name: "  Survivor One ".into(),
            skip_intro: true,
            no_pause: true,
            windowed: true,
            empty_world: true,
            cpu_count: Some(6),
            extra_args: "-exThreads=7 -profiles=\"C:\\My Profiles\"".into(),
            ..Default::default()
        };
        assert_eq!(
            option_args(&all),
            [
                "-name=Survivor One",
                "-nosplash",
                "-skipIntro",
                "-noPause",
                "-window",
                "-world=empty",
                "-cpuCount=6",
            ]
        );
        let added = added_args(&all).unwrap();
        assert_eq!(added.len(), 9);
        assert_eq!(added[7..], ["-exThreads=7", "-profiles=C:\\My Profiles"]);
        assert_eq!(shown_args(&added[..2]), r#"-name="Survivor One" -nosplash"#);
        // An unclosed quote in the extra arguments fails the launch, options and all.
        let broken = Settings {
            extra_args: "-x=\"open".into(),
            ..all
        };
        assert!(added_args(&broken).is_err());
        // No cores is no setting.
        let zero = Settings {
            cpu_count: Some(0),
            ..Default::default()
        };
        assert!(option_args(&zero).is_empty());
    }

    #[test]
    fn names_that_could_break_the_command_line() {
        assert_eq!(clean_name("  "), "");
        assert_eq!(clean_name("\t\n"), "");
        assert_eq!(clean_name("[TAG] Bob_99"), "[TAG] Bob_99");
        assert_eq!(clean_name("Zoë (medic)"), "Zoë (medic)");
        assert_eq!(clean_name(r#" Bob "the Builder" "#), "Bob the Builder");
        assert_eq!(clean_name("a'b\\c`d$e;f&g|h<i>j"), "abcdefghij");
        assert_eq!(clean_name("x\u{7}y\u{0}z"), "xyz");
        // Left with nothing but space: no name at all.
        let quotes = Settings {
            name: " \"\" ".into(),
            ..Default::default()
        };
        assert!(option_args(&quotes).is_empty());
        // A name is one argument, spaces and all.
        let spaced = Settings {
            name: "Survivor  One".into(),
            ..Default::default()
        };
        assert_eq!(option_args(&spaced), ["-name=Survivor  One"]);
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
        // The launch options count towards the limit too.
        let options = Settings {
            name: "Survivor One".into(),
            skip_intro: true,
            no_pause: true,
            windowed: true,
            empty_world: true,
            cpu_count: Some(4),
            extra_args: "-exThreads=7".into(),
            ..Default::default()
        };
        let added = added_args(&options).unwrap();
        let launch = build_steam(
            vec!["steam".into()],
            &install,
            &server,
            &[1559212036],
            None,
            &added,
            &state,
        )
        .unwrap();
        assert_eq!(launch.args[5..], added[..]);
        assert!(launch.args.contains(&"-name=Survivor One".to_string()));
        let base: usize = launch.args.iter().map(|a| a.len() + 1).sum::<usize>()
            - added.iter().map(|a| a.len() + 1).sum::<usize>();
        let fill = |name_len: usize| Settings {
            name: "n".repeat(name_len),
            ..options.clone()
        };
        let room = STEAM_ARGS_LIMIT - base - "-name=".len() - 1;
        let others: usize = added[1..].iter().map(|a| a.len() + 1).sum();
        let just_fits = added_args(&fill(room - others)).unwrap();
        build_steam(
            vec!["steam".into()],
            &install,
            &server,
            &[1559212036],
            None,
            &just_fits,
            &state,
        )
        .unwrap();
        let too_long = added_args(&fill(room - others + 1)).unwrap();
        let refused = build_steam(
            vec!["steam".into()],
            &install,
            &server,
            &[1559212036],
            None,
            &too_long,
            &state,
        );
        assert!(refused.unwrap_err().to_string().contains("or the name"));
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
