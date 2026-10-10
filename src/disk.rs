//! Disk space for mod downloads: how much the Workshop library's drive has free, whether a
//! download fits, and sizes written for people.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

const GB: u64 = 1_000_000_000;

/// Sizes the way Steam shows them (decimal units): "640 KB", "85 MB", "3.2 GB". Each unit starts
/// where the smaller one would round up to 1,000 (or 100.0 GB).
pub fn format_size(bytes: u64) -> String {
    let b = bytes as f64;
    if bytes >= 99_950_000_000 {
        format!("{:.0} GB", b / 1e9)
    } else if bytes >= 999_500_000 {
        format!("{:.1} GB", b / 1e9)
    } else if bytes >= 999_500 {
        format!("{:.0} MB", b / 1e6)
    } else if bytes >= 1_000 {
        format!("{:.0} KB", b / 1e3)
    } else {
        format!("{bytes} bytes")
    }
}

/// The free space a download should leave: Steam unpacks and moves files around while it
/// installs, so a tenth of the download, and at least 1 GB.
pub fn spare(needed: u64) -> u64 {
    (needed / 10).max(GB)
}

/// Whether a download of `needed` bytes fits in `free` bytes, with room to spare.
pub fn fits(needed: u64, free: u64) -> bool {
    needed
        .checked_add(spare(needed))
        .is_some_and(|wanted| free >= wanted)
}

/// Free space on a drive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Free {
    /// Bytes this user may still write there.
    pub bytes: u64,
    /// Where the drive is mounted (Linux) or its root (Windows), for messages.
    pub drive: PathBuf,
}

/// The free space on the drive holding `path`. The path itself may not exist yet (the Workshop
/// folder appears with the first mod), so this asks about its nearest existing parent.
pub fn free_space(path: &Path) -> Result<Free> {
    let existing = path
        .ancestors()
        .find(|p| !p.as_os_str().is_empty() && p.exists())
        .with_context(|| format!("{} doesn't exist", path.display()))?;
    let bytes = fs4::available_space(existing)
        .with_context(|| format!("couldn't read the free space on {}", existing.display()))?;
    Ok(Free {
        bytes,
        drive: drive(existing),
    })
}

/// Where the file system holding `path` (which exists) is mounted: the last parent on the same
/// device.
#[cfg(unix)]
fn drive(path: &Path) -> PathBuf {
    use std::os::unix::fs::MetadataExt;
    let path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let device = |p: &Path| std::fs::metadata(p).map(|m| m.dev()).ok();
    let Some(own) = device(&path) else {
        return path;
    };
    let mut top = path.as_path();
    while let Some(parent) = top.parent() {
        if device(parent) != Some(own) {
            break;
        }
        top = parent;
    }
    top.to_path_buf()
}

/// The drive holding `path`: its prefix and root, such as `D:\`.
#[cfg(windows)]
fn drive(path: &Path) -> PathBuf {
    use std::path::Component;
    path.components()
        .take_while(|c| matches!(c, Component::Prefix(_) | Component::RootDir))
        .collect()
}

#[cfg(not(any(unix, windows)))]
fn drive(path: &Path) -> PathBuf {
    path.to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes() {
        assert_eq!(format_size(0), "0 bytes");
        assert_eq!(format_size(999), "999 bytes");
        assert_eq!(format_size(640_000), "640 KB");
        assert_eq!(format_size(85_400_000), "85 MB");
        assert_eq!(format_size(3_240_000_000), "3.2 GB");
        assert_eq!(format_size(999_999_999), "1.0 GB");
        assert_eq!(format_size(999_999), "1 MB");
        assert_eq!(format_size(99_960_000_000), "100 GB");
        assert_eq!(format_size(1_000_000_000), "1.0 GB");
        assert_eq!(format_size(250_400_000_000), "250 GB");
    }

    #[test]
    fn room_to_spare() {
        // Small downloads keep at least 1 GB free.
        assert_eq!(spare(50_000_000), GB);
        assert!(!fits(50_000_000, 900_000_000));
        assert!(fits(50_000_000, 1_050_000_000));
        // Big ones keep a tenth more.
        assert_eq!(spare(30 * GB), 3 * GB);
        assert!(fits(30 * GB, 33 * GB));
        assert!(!fits(30 * GB, 33 * GB - 1));
        assert!(fits(0, GB));
        assert!(!fits(u64::MAX, u64::MAX));
    }

    #[test]
    fn free_space_of_a_folder_not_made_yet() {
        let dir = std::env::temp_dir().join(format!("dzb-disk-{}", std::process::id()));
        let free = free_space(&dir.join("steamapps/workshop/content/221100")).unwrap();
        let direct = free_space(&std::env::temp_dir()).unwrap();
        assert_eq!(free.drive, direct.drive);
        assert!(free.drive.has_root());
        #[cfg(unix)]
        assert!(
            std::env::temp_dir()
                .canonicalize()
                .unwrap()
                .starts_with(&free.drive)
        );
    }

    #[test]
    #[cfg(windows)]
    fn windows_drive() {
        assert_eq!(
            drive(Path::new(r"D:\SteamLibrary\steamapps")),
            Path::new(r"D:\")
        );
    }
}
