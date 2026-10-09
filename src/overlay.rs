//! The DayZ map overlay (`dayz-map`): built from the `overlay` submodule and shipped next to the
//! browser, but still its own program. It's started in the background while the browser runs,
//! given its hotkey through its own config file, and driven only through its command line, so
//! the two stay independent.

use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::settings::write_atomic;

const EXE: &str = if cfg!(windows) {
    "dayz-map.exe"
} else {
    "dayz-map"
};

/// The overlay program, which ships next to this one (as `cargo build` also leaves it).
pub fn program() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let path = exe.parent()?.join(EXE);
    path.is_file().then_some(path)
}

fn quiet(program: &Path) -> Command {
    let mut command = Command::new(program);
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(crate::win::CREATE_NO_WINDOW);
    }
    command
}

/// Whether the overlay is running. It answers `hide` only when it is (and it's hidden whenever
/// the game isn't in front, so asking changes nothing).
pub fn is_running(program: &Path) -> bool {
    quiet(program)
        .arg("hide")
        .status()
        .is_ok_and(|s| s.success())
}

/// Starts the overlay in the background, independent of the browser (it keeps running if the
/// browser closes).
pub fn start(program: &Path) -> Result<()> {
    let mut command = quiet(program);
    command.arg("run");
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
    // Collect its exit status whenever it quits, so it doesn't linger as a zombie.
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

/// Quits the overlay and starts it again, so it reads its config anew.
pub fn restart(program: &Path) -> Result<()> {
    if is_running(program) {
        let _ = quiet(program).arg("quit").status();
        let deadline = Instant::now() + Duration::from_secs(5);
        while is_running(program) {
            if Instant::now() > deadline {
                bail!("the overlay didn't quit");
            }
            std::thread::sleep(Duration::from_millis(200));
        }
    }
    start(program)
}

/// The overlay's config file (it uses the same per-user folders as the browser).
pub fn config_path() -> PathBuf {
    directories::ProjectDirs::from("", "", "dayz-map-overlay")
        .expect("home directory is known")
        .config_dir()
        .join("config.toml")
}

/// The overlay's hotkey, as written in its config (`m` by default).
pub fn hotkey() -> String {
    std::fs::read_to_string(config_path())
        .ok()
        .and_then(|text| text.parse::<toml_edit::DocumentMut>().ok())
        .and_then(|doc| {
            doc.get("hotkey")
                .and_then(|v| v.as_str())
                .map(str::to_string)
        })
        .unwrap_or_else(|| "m".into())
}

/// Sets the overlay's hotkey in its config, keeping everything else in the file as it is.
pub fn set_hotkey(key: &str) -> Result<()> {
    let path = config_path();
    let mut doc = match std::fs::read_to_string(&path) {
        Ok(text) => text
            .parse::<toml_edit::DocumentMut>()
            .with_context(|| format!("{} is unreadable; not changing it", path.display()))?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => toml_edit::DocumentMut::new(),
        Err(e) => return Err(e).with_context(|| format!("couldn't read {}", path.display())),
    };
    doc["hotkey"] = toml_edit::value(key);
    write_atomic(&path, doc.to_string().as_bytes())
}

/// The overlay's name for a key the browser can capture, if the overlay supports it on both
/// Linux and Windows: a letter, a digit, F1-F24, Tab, or the backtick key.
pub fn key_name(key: eframe::egui::Key) -> Option<String> {
    use eframe::egui::Key;
    let name = key.name();
    if name.len() == 1 && name.chars().all(|c| c.is_ascii_alphanumeric()) {
        return Some(name.to_lowercase());
    }
    if let Some(n) = name.strip_prefix('F').and_then(|n| n.parse::<u32>().ok())
        && (1..=24).contains(&n)
    {
        return Some(format!("f{n}"));
    }
    match key {
        Key::Tab => Some("tab".into()),
        Key::Backtick => Some("grave".into()),
        _ => None,
    }
}

/// How to show a hotkey name: `M`, `F5`, `Tab`, `` ` ``.
pub fn key_label(name: &str) -> String {
    match name {
        "tab" => "Tab".into(),
        "grave" | "backtick" => "`".into(),
        other => other.to_uppercase(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use eframe::egui::Key;

    #[test]
    fn key_names_match_the_overlay() {
        assert_eq!(key_name(Key::M).as_deref(), Some("m"));
        assert_eq!(key_name(Key::Num5).as_deref(), Some("5"));
        assert_eq!(key_name(Key::F12).as_deref(), Some("f12"));
        assert_eq!(key_name(Key::Tab).as_deref(), Some("tab"));
        assert_eq!(key_name(Key::Backtick).as_deref(), Some("grave"));
        assert_eq!(key_name(Key::Escape), None);
        assert_eq!(key_name(Key::F30), None);
        assert_eq!(key_label("f5"), "F5");
    }
}
