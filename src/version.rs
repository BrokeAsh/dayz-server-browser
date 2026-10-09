//! The installed game's version, to compare with servers'. Servers write it as `1.29.163709`; the
//! game's own file version reads 1.29.163.709 (the build number split after its third digit).

use anyhow::{Context, Result};
use std::path::Path;

/// The version of the DayZ in this folder, written the way servers write theirs.
pub fn installed(game: &Path) -> Result<String> {
    let exe = game.join("DayZ_x64.exe");
    let bytes = std::fs::read(&exe).with_context(|| format!("couldn't read {}", exe.display()))?;
    file_version(&bytes).context("the game's version wasn't in its program file")
}

/// Reads a Windows program's version resource (`VS_FIXEDFILEINFO`) without parsing the whole
/// file: its signature is followed by the structure version and then the file version.
fn file_version(bytes: &[u8]) -> Option<String> {
    const SIGNATURE: [u8; 4] = 0xFEEF_04BD_u32.to_le_bytes();
    const STRUCTURE: [u8; 4] = 0x0001_0000_u32.to_le_bytes();
    let word = |at: usize| -> Option<u32> {
        Some(u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
    };
    let at = bytes
        .windows(8)
        .position(|w| w[..4] == SIGNATURE && w[4..] == STRUCTURE)?;
    let (high, low) = (word(at + 8)?, word(at + 12)?);
    let build = (low >> 16) * 1000 + (low & 0xffff);
    Some(format!("{}.{}.{build}", high >> 16, high & 0xffff))
}

/// Whether a server runs this version of the game; `None` if either is unknown.
pub fn same(game: &str, server: &str) -> Option<bool> {
    let parse = |v: &str| -> Option<Vec<u32>> {
        let parts: Option<Vec<u32>> = v.trim().split('.').map(|p| p.parse().ok()).collect();
        parts.filter(|p| p.len() == 3)
    };
    Some(parse(game)? == parse(server)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_file_version() {
        let mut exe = vec![0u8; 40];
        exe.extend_from_slice(&0xFEEF_04BD_u32.to_le_bytes());
        exe.extend_from_slice(&0x0001_0000_u32.to_le_bytes());
        exe.extend_from_slice(&((1 << 16) | 29_u32).to_le_bytes());
        exe.extend_from_slice(&((163 << 16) | 47_u32).to_le_bytes());
        exe.extend_from_slice(&[0; 16]);
        assert_eq!(file_version(&exe).as_deref(), Some("1.29.163047"));
        assert_eq!(file_version(&[0; 64]), None);
    }

    #[test]
    fn compares_versions() {
        assert_eq!(same("1.29.163709", "1.29.163709"), Some(true));
        assert_eq!(same("1.29.163709", "1.29.163047"), Some(false));
        assert_eq!(same("1.29.163709", "1.28.161464"), Some(false));
        assert_eq!(same("1.29.163709", ""), None);
        assert_eq!(same("1.29.163709", "1.29"), None);
    }
}
