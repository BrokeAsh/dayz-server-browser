//! Checks before joining: how much the missing and outdated mods add up to, and whether the
//! Workshop library's drive has room for them.

use eframe::egui;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::disk::{self, Free};
use crate::steam_api::FileDetails;

/// How often to look at the free space again while the join window is open.
const SPACE_EVERY: Duration = Duration::from_secs(5);

/// What the mods to download or update add up to.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct Downloads {
    /// Bytes, of the mods whose size is known.
    pub bytes: u64,
    /// Mods whose size isn't known (details not fetched yet, or Steam didn't say).
    pub unknown: usize,
}

/// The total size of these Workshop mods. An update counts in full: Steam downloads the whole new
/// version next to the old one before swapping them.
pub(super) fn downloads(ids: &[u64], details: &HashMap<u64, FileDetails>) -> Downloads {
    let mut total = Downloads::default();
    let mut seen = std::collections::HashSet::new();
    // A mod listed twice is downloaded once.
    for id in ids.iter().filter(|id| seen.insert(**id)) {
        match details.get(id).and_then(|d| d.size) {
            Some(size) => total.bytes = total.bytes.saturating_add(size),
            None => total.unknown += 1,
        }
    }
    total
}

impl Downloads {
    /// "Downloads 3.2 GB", or what's known of it.
    pub fn describe(&self) -> String {
        let size = disk::format_size(self.bytes);
        match (self.bytes, self.unknown) {
            (_, 0) => format!("Downloads {size}"),
            (0, _) => "Download size unknown".into(),
            (_, 1) => format!("Downloads {size} or more (1 size unknown)"),
            (_, n) => format!("Downloads {size} or more ({n} sizes unknown)"),
        }
    }
}

/// Whether the downloads fit on the drive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Space {
    /// Nothing known to download, or the free space isn't known: no reason to stop anything.
    Unknown,
    Fits,
    /// Too little room for the downloads with some to spare.
    Short {
        needed: u64,
        free: Free,
    },
}

/// Compares the downloads with the free space. Sizes not known don't count, so a shortfall is
/// certain (the known ones alone don't fit) and unknown sizes never block a download.
pub(super) fn space(downloads: &Downloads, free: Option<&Free>) -> Space {
    let (Some(free), needed @ 1..) = (free, downloads.bytes) else {
        return Space::Unknown;
    };
    if disk::fits(needed, free.bytes) {
        Space::Fits
    } else {
        Space::Short {
            needed,
            free: free.clone(),
        }
    }
}

impl Space {
    /// The warning for a shortfall: "Needs 3.2 GB plus 1.0 GB to spare; 1.1 GB free on /mnt/storage".
    pub fn warning(&self) -> Option<String> {
        let Space::Short { needed, free } = self else {
            return None;
        };
        Some(format!(
            "Needs {} plus {} to spare; {} free on {}",
            disk::format_size(*needed),
            disk::format_size(disk::spare(*needed)),
            disk::format_size(free.bytes),
            free.drive.display()
        ))
    }
}

type Answer = (PathBuf, Result<Free, String>);

/// Looks at the free space on the Workshop library's drive every few seconds, on a thread (a slow
/// or network drive mustn't hold up the window).
#[derive(Default)]
pub(super) struct SpaceProbe {
    latest: Option<Answer>,
    asked: Option<Instant>,
    /// Filled in by the thread; while it holds a `None`, the thread hasn't answered yet.
    answer: Option<Arc<Mutex<Option<Answer>>>>,
}

impl SpaceProbe {
    /// Picks up an answer, and asks again when it's time (or the folder changed).
    pub fn poll(&mut self, ctx: &egui::Context, folder: &Path) {
        if let Some(slot) = &self.answer {
            let Some(answer) = slot.lock().ok().and_then(|mut a| a.take()) else {
                return;
            };
            if let Err(e) = &answer.1 {
                log::debug!("free space: {e}");
            }
            self.latest = Some(answer);
            self.answer = None;
        }
        let same_folder = self.latest.as_ref().is_some_and(|(f, _)| f == folder);
        if same_folder && self.asked.is_some_and(|t| t.elapsed() < SPACE_EVERY) {
            return;
        }
        let slot = Arc::new(Mutex::new(None));
        self.answer = Some(slot.clone());
        self.asked = Some(Instant::now());
        let (folder, ctx) = (folder.to_path_buf(), ctx.clone());
        std::thread::spawn(move || {
            let free = disk::free_space(&folder).map_err(|e| format!("{e:#}"));
            if let Ok(mut slot) = slot.lock() {
                *slot = Some((folder, free));
            }
            ctx.request_repaint();
        });
    }

    /// The latest free space for this folder, if it was read.
    pub fn free(&self, folder: &Path) -> Option<&Free> {
        match &self.latest {
            Some((f, Ok(free))) if f == folder => Some(free),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GB: u64 = 1_000_000_000;

    fn details(sizes: &[(u64, Option<u64>)]) -> HashMap<u64, FileDetails> {
        sizes
            .iter()
            .map(|&(id, size)| {
                (
                    id,
                    FileDetails {
                        time_updated: 1,
                        size,
                        title: String::new(),
                    },
                )
            })
            .collect()
    }

    fn free(bytes: u64) -> Free {
        Free {
            bytes,
            drive: PathBuf::from("/mnt/storage"),
        }
    }

    #[test]
    fn adds_up_the_downloads() {
        let known = details(&[(1, Some(2 * GB)), (2, Some(1_200_000_000)), (3, None)]);
        assert_eq!(
            downloads(&[1, 2], &known),
            Downloads {
                bytes: 3_200_000_000,
                unknown: 0
            }
        );
        assert_eq!(downloads(&[1, 2], &known).describe(), "Downloads 3.2 GB");
        assert_eq!(downloads(&[1, 2, 1], &known), downloads(&[1, 2], &known));
        // Steam didn't say, or the details aren't in yet.
        let some = downloads(&[1, 3, 4], &known);
        assert_eq!(
            some,
            Downloads {
                bytes: 2 * GB,
                unknown: 2
            }
        );
        assert_eq!(
            some.describe(),
            "Downloads 2.0 GB or more (2 sizes unknown)"
        );
        assert_eq!(
            downloads(&[1, 3], &known).describe(),
            "Downloads 2.0 GB or more (1 size unknown)"
        );
        let none = downloads(&[3, 4], &HashMap::new());
        assert_eq!(none.describe(), "Download size unknown");
        assert_eq!(downloads(&[], &known), Downloads::default());
    }

    #[test]
    fn decides_whether_there_is_room() {
        let need = Downloads {
            bytes: 3_200_000_000,
            unknown: 0,
        };
        // 3.2 GB plus 1 GB to spare.
        assert_eq!(space(&need, Some(&free(4_200_000_000))), Space::Fits);
        let short = space(&need, Some(&free(1_100_000_000)));
        assert_eq!(
            short.warning().unwrap(),
            "Needs 3.2 GB plus 1.0 GB to spare; 1.1 GB free on /mnt/storage"
        );
        // Enough for the download itself, but not the room to spare.
        assert!(matches!(
            space(&need, Some(&free(4 * GB))),
            Space::Short { .. }
        ));
        // Big downloads keep a tenth spare.
        let big = Downloads {
            bytes: 40 * GB,
            unknown: 0,
        };
        assert_eq!(space(&big, Some(&free(44 * GB))), Space::Fits);
        assert!(matches!(
            space(&big, Some(&free(44 * GB - 1))),
            Space::Short { .. }
        ));
        // Unknown free space or sizes: never in the way.
        assert_eq!(space(&need, None), Space::Unknown);
        let unknown = Downloads {
            bytes: 0,
            unknown: 3,
        };
        assert_eq!(space(&unknown, Some(&free(0))), Space::Unknown);
        assert_eq!(Space::Unknown.warning(), None);
        // The known part alone doesn't fit: that's certain whatever the rest weighs.
        let partly = Downloads {
            bytes: 3_200_000_000,
            unknown: 2,
        };
        assert!(matches!(
            space(&partly, Some(&free(GB))),
            Space::Short { .. }
        ));
    }
}
