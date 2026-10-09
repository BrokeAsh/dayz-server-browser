//! The user's Steam Web API key: kept in the system keyring (KWallet or GNOME Keyring through the
//! Secret Service, Windows Credential Manager), or, where there's no keyring, in a file only this
//! user can read. Never in the config file or logs.

use anyhow::{Context, Result};
use std::path::PathBuf;

use crate::settings::config_dir;

const SERVICE: &str = "dayz-browser";
const ACCOUNT: &str = "steam-web-api-key";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Storage {
    Keyring,
    File,
}

impl Storage {
    pub fn describe(self) -> &'static str {
        match self {
            Storage::Keyring => "the system keyring",
            Storage::File => "a private file (no keyring found)",
        }
    }
}

fn entry() -> Option<keyring::Entry> {
    keyring::Entry::new(SERVICE, ACCOUNT).ok()
}

fn file() -> PathBuf {
    config_dir().join("steam-key")
}

/// The saved key and where it is, if there is one.
pub fn load() -> Option<(String, Storage)> {
    if let Some(entry) = entry()
        && let Ok(key) = entry.get_password()
    {
        return Some((key, Storage::Keyring));
    }
    let key = std::fs::read_to_string(file()).ok()?;
    let key = key.trim().to_string();
    (!key.is_empty()).then_some((key, Storage::File))
}

/// Saves the key, in the keyring if possible.
pub fn save(key: &str) -> Result<Storage> {
    if let Some(entry) = entry()
        && entry.set_password(key).is_ok()
    {
        let _ = std::fs::remove_file(file());
        return Ok(Storage::Keyring);
    }
    write_private(key).context("couldn't save the key")?;
    Ok(Storage::File)
}

pub fn remove() {
    if let Some(entry) = entry() {
        let _ = entry.delete_credential();
    }
    let _ = std::fs::remove_file(file());
}

#[cfg(unix)]
fn write_private(key: &str) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::create_dir_all(config_dir())?;
    let tmp = file().with_extension("tmp");
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&tmp)?;
    f.write_all(key.as_bytes())?;
    f.sync_all()?;
    std::fs::rename(tmp, file())
}

#[cfg(windows)]
fn write_private(key: &str) -> std::io::Result<()> {
    // The roaming AppData folder is only readable by this user by default.
    std::fs::create_dir_all(config_dir())?;
    std::fs::write(file(), key)
}
