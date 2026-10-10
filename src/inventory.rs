//! What's in DayZ's Workshop folder, for the Mods page: the mods Steam installed (with their
//! names, sizes and update times), the folders it didn't (left behind, or another account's not
//! installed yet), which of the user's servers use each mod, and deleting a leftover folder
//! safely.

use anyhow::{Context, Result, bail};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::steam::{self, Install};
use crate::vdf;

/// Steam ids of individual accounts are this plus the 32-bit account id Steam's files use.
const STEAM_ID_BASE: u64 = 76561197960265728;

/// Steam's record of one Workshop item (`appworkshop_221100.acf`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Record {
    /// Listed under `WorkshopItemsInstalled`.
    pub installed: bool,
    /// Bytes on disk, as Steam counted them.
    pub size: Option<u64>,
    /// Unix time of the version installed.
    pub time_updated: u64,
    /// The account (32-bit id) that subscribed to it, from `WorkshopItemDetails`.
    pub subscribed_by: Option<u32>,
}

/// Steam's records, or `None` if the file is missing or unreadable (and so nothing can be called
/// a leftover).
pub fn read_records(text: &str) -> Option<HashMap<u64, Record>> {
    let root = vdf::parse(text);
    let app = root.get("AppWorkshop")?;
    let installed = app.get("WorkshopItemsInstalled");
    let details = app.get("WorkshopItemDetails");
    // A file without either section isn't a record to trust.
    if installed.is_none() && details.is_none() {
        return None;
    }
    let mut records: HashMap<u64, Record> = HashMap::new();
    for (id, entry) in installed.map(vdf::Value::entries).unwrap_or_default() {
        let Ok(id) = id.parse::<u64>() else { continue };
        let record = records.entry(id).or_default();
        record.installed = true;
        record.size = entry.str("size").and_then(|s| s.parse().ok());
        record.time_updated = entry
            .str("timeupdated")
            .and_then(|t| t.parse().ok())
            .unwrap_or(0);
    }
    for (id, entry) in details.map(vdf::Value::entries).unwrap_or_default() {
        let Ok(id) = id.parse::<u64>() else { continue };
        records.entry(id).or_default().subscribed_by = entry
            .str("subscribedby")
            .and_then(|s| s.parse().ok())
            .filter(|&a| a != 0);
    }
    Some(records)
}

/// The Steam account signed in most recently (its 32-bit id) to the Steam that owns DayZ's
/// library, from its `loginusers.vdf`. If no Steam install lists that library (or none of those
/// says who signed in), the one signed in last to any of them.
pub fn current_account(install: &Install) -> Option<u32> {
    let roots: Vec<PathBuf> = steam::steam_roots()
        .into_iter()
        .map(|(root, _)| root)
        .collect();
    let library = install
        .game
        .parent()
        .and_then(Path::parent)
        .and_then(Path::parent)?;
    account_for(&roots, library)
}

/// [`current_account`] among these Steam installs, for the library (the folder holding
/// `steamapps`) DayZ is in.
fn account_for(roots: &[PathBuf], library: &Path) -> Option<u32> {
    let latest = |roots: &mut dyn Iterator<Item = &PathBuf>| {
        roots
            .filter_map(|root| {
                let path = root.join("config").join("loginusers.vdf");
                account_from_login_users(&std::fs::read_to_string(path).ok()?)
            })
            .max_by_key(|&(_, time)| time)
            .map(|(account, _)| account)
    };
    latest(&mut roots.iter().filter(|root| owns_library(root, library)))
        .or_else(|| latest(&mut roots.iter()))
}

/// Whether a Steam install keeps its games in `library`: it's the install's own folder, or one its
/// `libraryfolders.vdf` lists.
fn owns_library(root: &Path, library: &Path) -> bool {
    let same = |a: &Path| match (a.canonicalize(), library.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    };
    if same(root) {
        return true;
    }
    let Ok(text) = std::fs::read_to_string(root.join("steamapps").join("libraryfolders.vdf"))
    else {
        return false;
    };
    let parsed = vdf::parse(&text);
    parsed.get("libraryfolders").is_some_and(|folders| {
        folders
            .entries()
            .iter()
            .filter_map(|(_, folder)| folder.str("path"))
            .any(|path| same(Path::new(path)))
    })
}

/// The account marked `MostRecent`, or else the one with the latest sign-in time; with its
/// sign-in time.
fn account_from_login_users(text: &str) -> Option<(u32, u64)> {
    let root = vdf::parse(text);
    let users = root.get("users")?;
    let mut best: Option<(bool, u64, u32)> = None;
    for (steam_id, user) in users.entries() {
        let Some(account) = steam_id
            .parse::<u64>()
            .ok()
            .and_then(|id| id.checked_sub(STEAM_ID_BASE))
            .and_then(|a| u32::try_from(a).ok())
            .filter(|&a| a != 0)
        else {
            continue;
        };
        let most_recent = user.str("MostRecent") == Some("1");
        let time = user
            .str("Timestamp")
            .and_then(|t| t.parse().ok())
            .unwrap_or(0);
        let candidate = (most_recent, time, account);
        if best.is_none_or(|b| (candidate.0, candidate.1) > (b.0, b.1)) {
            best = Some(candidate);
        }
    }
    best.map(|(_, time, account)| (account, time))
}

/// Why a folder counts as left over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Leftover {
    /// Steam has no record of installing it.
    NoRecord,
    /// Not installed, and another Steam account on this computer subscribed to it.
    OtherAccount(u32),
}

/// Whether a folder with this record is a leftover; `account` is the current one, if known.
///
/// A mod Steam lists as installed never is, whoever it's recorded under: Steam keeps a single
/// subscriber per mod, so one that two accounts on this computer use shows only one of them
/// (see [`other_account`]).
pub fn classify(record: Option<&Record>, account: Option<u32>) -> Option<Leftover> {
    match record {
        Some(r) if r.installed => None,
        // Subscribed and not installed yet (Steam is still getting it): this account's unless
        // Steam says another's.
        Some(r) if r.subscribed_by.is_some() => match (r.subscribed_by, account) {
            (Some(by), Some(me)) if by != me => Some(Leftover::OtherAccount(by)),
            _ => None,
        },
        _ => Some(Leftover::NoRecord),
    }
}

/// The other Steam account an installed mod is recorded under, if it's not the current one. The
/// current account may well use it too.
pub fn other_account(record: Option<&Record>, account: Option<u32>) -> Option<u32> {
    let r = record.filter(|r| r.installed)?;
    r.subscribed_by
        .filter(|&by| account.is_some_and(|me| me != by))
}

/// One numeric folder in the Workshop folder.
#[derive(Debug, Clone)]
pub struct Folder {
    pub id: u64,
    pub path: PathBuf,
    /// The name in its `meta.cpp`.
    pub name: Option<String>,
    /// Bytes: Steam's count for an installed mod, otherwise counted here.
    pub size: u64,
    /// Unix time of the version installed (0 when Steam didn't record one).
    pub time_updated: u64,
    /// It's a link (or on Windows, a junction), so it's never deleted through.
    pub link: bool,
    /// `None` for a mod Steam installed.
    pub leftover: Option<Leftover>,
    /// An installed mod Steam recorded under another account on this computer ([`other_account`]).
    pub other_account: Option<u32>,
}

/// Everything in the Workshop folder.
#[derive(Debug, Clone, Default)]
pub struct Inventory {
    pub installed: Vec<Folder>,
    pub leftovers: Vec<Folder>,
    /// Steam's records couldn't be read, so no folder is called a leftover.
    pub no_records: bool,
    /// The Steam account the leftovers were judged against.
    pub account: Option<u32>,
}

impl Inventory {
    pub fn get(&self, id: u64) -> Option<&Folder> {
        self.installed
            .iter()
            .chain(&self.leftovers)
            .find(|f| f.id == id)
    }
}

/// A Workshop item's folder name: digits only (and a plausible id).
fn item_id(name: &str) -> Option<u64> {
    if name.is_empty() || name.len() > 20 || !name.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    name.parse().ok().filter(|&id| id != 0)
}

/// Reads the Workshop folder and Steam's records. This counts the size of every leftover folder,
/// so it belongs on a background thread.
pub fn scan(install: &Install, account: Option<u32>) -> Inventory {
    let records = std::fs::read_to_string(&install.manifest)
        .ok()
        .and_then(|text| read_records(&text));
    let mut inventory = Inventory {
        no_records: records.is_none(),
        account,
        ..Default::default()
    };
    let Ok(entries) = std::fs::read_dir(&install.workshop) else {
        return inventory;
    };
    for entry in entries.flatten() {
        let Some(id) = entry.file_name().to_str().and_then(item_id) else {
            continue;
        };
        let path = entry.path();
        let link = is_link(&path);
        let dir = if link {
            path.is_dir()
        } else {
            entry.file_type().is_ok_and(|t| t.is_dir())
        };
        if !dir {
            continue;
        }
        let record = records.as_ref().and_then(|r| r.get(&id));
        let leftover = match &records {
            Some(_) => classify(record, account),
            None => None,
        };
        let steam_size = record
            .filter(|r| r.installed)
            .and_then(|r| r.size)
            .filter(|&s| s > 0);
        let folder = Folder {
            id,
            name: meta_name(&path),
            size: steam_size.unwrap_or_else(|| folder_size(&path)),
            time_updated: record.map_or(0, |r| r.time_updated),
            link,
            leftover,
            other_account: other_account(record, account),
            path,
        };
        if folder.leftover.is_some() {
            inventory.leftovers.push(folder);
        } else {
            inventory.installed.push(folder);
        }
    }
    inventory
}

/// The `name` in a mod's `meta.cpp` (as `name = "...";`), however the file name is capitalised.
pub fn meta_name(dir: &Path) -> Option<String> {
    let file = std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .find(|e| e.file_name().eq_ignore_ascii_case("meta.cpp"))?;
    let bytes = std::fs::read(file.path()).ok()?;
    if bytes.len() > 64 * 1024 {
        return None;
    }
    parse_meta_name(&String::from_utf8_lossy(&bytes))
}

fn parse_meta_name(text: &str) -> Option<String> {
    text.lines().find_map(|line| {
        let (key, value) = line.split_once('=')?;
        if !key.trim().eq_ignore_ascii_case("name") {
            return None;
        }
        let value = value.trim().trim_end_matches(';').trim();
        let value = value.strip_prefix('"').unwrap_or(value);
        let value = value.strip_suffix('"').unwrap_or(value).trim();
        (!value.is_empty()).then(|| value.to_string())
    })
}

/// The bytes in a folder's files, without following links.
pub fn folder_size(dir: &Path) -> u64 {
    let mut total = 0;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(meta) = std::fs::symlink_metadata(entry.path()) else {
                continue;
            };
            if meta.file_type().is_symlink() || is_reparse_point(&meta) {
                continue;
            }
            if meta.is_dir() {
                stack.push(entry.path());
            } else {
                total += meta.len();
            }
        }
    }
    total
}

/// Whether a path is a link (or, on Windows, any reparse point such as a junction).
fn is_link(path: &Path) -> bool {
    std::fs::symlink_metadata(path)
        .is_ok_and(|meta| meta.file_type().is_symlink() || is_reparse_point(&meta))
}

#[cfg(windows)]
fn is_reparse_point(meta: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
    meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

#[cfg(not(windows))]
fn is_reparse_point(_meta: &std::fs::Metadata) -> bool {
    false
}

/// For each mod, which of these servers (by their place in `lists`) use it. Each list is one
/// server's mods.
pub fn used_by(lists: &[Vec<u64>]) -> HashMap<u64, Vec<usize>> {
    let mut used: HashMap<u64, Vec<usize>> = HashMap::new();
    for (server, ids) in lists.iter().enumerate() {
        let unique: HashSet<u64> = ids.iter().copied().filter(|&id| id != 0).collect();
        for id in unique {
            used.entry(id).or_default().push(server);
        }
    }
    used
}

/// The folder at `path`, checked to be safe to delete: a real folder (not a link) directly inside
/// DayZ's Workshop folder (`.../workshop/content/221100`), named as a Workshop item (digits only),
/// before and after resolving links. Returns its resolved path.
pub fn checked_folder(workshop: &Path, path: &Path) -> Result<PathBuf> {
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .context("the folder has no name")?;
    if item_id(name).is_none() {
        bail!("{} isn't a Workshop item's folder", path.display());
    }
    let app = workshop.file_name().and_then(|n| n.to_str());
    let content = workshop
        .parent()
        .and_then(Path::file_name)
        .and_then(|n| n.to_str());
    if app != Some(&steam::DAYZ_APP.to_string()) || content != Some("content") {
        bail!("{} isn't DayZ's Workshop folder", workshop.display());
    }
    if path.parent() != Some(workshop) {
        bail!("{} isn't in DayZ's Workshop folder", path.display());
    }
    let meta = std::fs::symlink_metadata(path)
        .with_context(|| format!("couldn't read {}", path.display()))?;
    if meta.file_type().is_symlink() || is_reparse_point(&meta) {
        bail!("{} is a link; not deleting through it", path.display());
    }
    if !meta.is_dir() {
        bail!("{} isn't a folder", path.display());
    }
    let real_workshop = workshop
        .canonicalize()
        .with_context(|| format!("couldn't resolve {}", workshop.display()))?;
    let real = path
        .canonicalize()
        .with_context(|| format!("couldn't resolve {}", path.display()))?;
    if real.parent() != Some(real_workshop.as_path())
        || real.file_name().and_then(|n| n.to_str()) != Some(name)
    {
        bail!("{} leads outside DayZ's Workshop folder", path.display());
    }
    Ok(real)
}

/// Deletes a leftover folder after checking again, from Steam's current records and the account
/// signed in now, that it's still one (Steam may have installed it meanwhile, or another account
/// signed in). Returns the bytes freed. This reads Steam's files, so it belongs on a background
/// thread.
pub fn delete_leftover(install: &Install, id: u64) -> Result<u64> {
    delete_leftover_for(install, id, current_account(install))
}

fn delete_leftover_for(install: &Install, id: u64, account: Option<u32>) -> Result<u64> {
    let text = std::fs::read_to_string(&install.manifest)
        .context("couldn't read Steam's record of installed mods")?;
    let records = read_records(&text).context("couldn't read Steam's record of installed mods")?;
    if classify(records.get(&id), account).is_none() {
        bail!("Steam has this mod installed (or is getting it) now, so it isn't left over");
    }
    let path = checked_folder(&install.workshop, &install.workshop.join(id.to_string()))?;
    let size = folder_size(&path);
    // This doesn't follow links inside the folder: it removes the links themselves.
    std::fs::remove_dir_all(&path)
        .with_context(|| format!("couldn't delete {}", path.display()))?;
    Ok(size)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ACF: &str = r#""AppWorkshop"
{
	"appid"		"221100"
	"WorkshopItemsInstalled"
	{
		"111"
		{
			"size"		"527656"
			"timeupdated"		"1771519119"
			"manifest"		"1"
		}
		"222"
		{
			"size"		"1000"
			"timeupdated"		"1700000000"
			"manifest"		"2"
		}
	}
	"WorkshopItemDetails"
	{
		"111"
		{
			"manifest"		"1"
			"subscribedby"		"88216642"
			"latest_manifest"		"1"
		}
		"222"
		{
			"subscribedby"		"5"
		}
		"444"
		{
			"subscribedby"		"88216642"
		}
		"777"
		{
			"subscribedby"		"5"
		}
	}
}"#;

    fn temp_install(name: &str) -> (Install, PathBuf) {
        let root = std::env::temp_dir().join(format!("dzb-inv-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let workshop = root.join("steamapps/workshop/content/221100");
        std::fs::create_dir_all(&workshop).unwrap();
        let install = Install {
            game: root.join("steamapps/common/DayZ"),
            workshop,
            manifest: root.join("steamapps/workshop/appworkshop_221100.acf"),
            how: "test",
        };
        (install, root)
    }

    fn make_mod(install: &Install, name: &str, bytes: usize) -> PathBuf {
        let dir = install.workshop.join(name);
        std::fs::create_dir_all(dir.join("addons")).unwrap();
        std::fs::write(dir.join("addons/a.pbo"), vec![0u8; bytes]).unwrap();
        std::fs::write(
            dir.join("meta.cpp"),
            format!("protocol = 1;\nname = \"Mod {name}\";\n"),
        )
        .unwrap();
        dir
    }

    #[test]
    fn reads_sizes_and_subscribers() {
        let records = read_records(ACF).unwrap();
        let a = &records[&111];
        assert!(a.installed);
        assert_eq!(a.size, Some(527656));
        assert_eq!(a.time_updated, 1771519119);
        assert_eq!(a.subscribed_by, Some(88216642));
        assert_eq!(records[&222].subscribed_by, Some(5));
        assert!(!records[&444].installed);
        assert_eq!(records[&444].size, None);
        assert!(read_records("").is_none(), "no file: no records");
        assert!(read_records(r#""AppWorkshop" { "appid" "221100" }"#).is_none());
    }

    #[test]
    fn finds_the_signed_in_account() {
        let users = r#""users" {
            "76561198048482370" { "AccountName" "a" "Timestamp" "1791394310" }
            "76561198154829661" { "AccountName" "b" "Timestamp" "1791045127" }
        }"#;
        assert_eq!(
            account_from_login_users(users),
            Some((88216642, 1791394310))
        );
        let marked = r#""users" {
            "76561198048482370" { "MostRecent" "0" "Timestamp" "9" }
            "76561198154829661" { "MostRecent" "1" "Timestamp" "1" }
        }"#;
        assert_eq!(account_from_login_users(marked), Some((194563933, 1)));
        assert_eq!(account_from_login_users(r#""users" { "12" { } }"#), None);
    }

    #[test]
    fn asks_the_steam_that_owns_the_library() {
        let root = std::env::temp_dir().join(format!("dzb-inv-roots-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let users = |steam: &Path, id: u64, time: u64| {
            std::fs::create_dir_all(steam.join("config")).unwrap();
            let text =
                format!(r#""users" {{ "{id}" {{ "MostRecent" "1" "Timestamp" "{time}" }} }}"#);
            std::fs::write(steam.join("config/loginusers.vdf"), text).unwrap();
        };
        // A stale native Steam, signed in more recently, beside the Flatpak that has DayZ in a
        // library of its own.
        let (native, flatpak, library) =
            (root.join("native"), root.join("flatpak"), root.join("lib"));
        users(&native, STEAM_ID_BASE + 1, 2000);
        users(&flatpak, STEAM_ID_BASE + 2, 1000);
        std::fs::create_dir_all(flatpak.join("steamapps")).unwrap();
        std::fs::create_dir_all(&library).unwrap();
        let roots = [native.clone(), flatpak.clone(), root.join("missing")];
        assert_eq!(
            account_for(&roots, &library),
            Some(1),
            "no owner: the latest"
        );
        std::fs::write(
            flatpak.join("steamapps/libraryfolders.vdf"),
            format!(
                r#""libraryfolders" {{ "0" {{ "path" "{}" }} "1" {{ "path" "{}" }} }}"#,
                flatpak.display(),
                library.display()
            ),
        )
        .unwrap();
        assert_eq!(
            account_for(&roots, &library),
            Some(2),
            "the library's owner"
        );
        // A library that's a Steam install's own folder.
        assert_eq!(account_for(&roots, &native), Some(1));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn reads_meta_names() {
        assert_eq!(
            parse_meta_name("protocol = 1;\nname = \"CF\";\ntimestamp = 5;"),
            Some("CF".into())
        );
        assert_eq!(
            parse_meta_name("Name=Plain words ;"),
            Some("Plain words".into())
        );
        assert_eq!(parse_meta_name("publishedid = 5;\nname = \"\";"), None);
        assert_eq!(parse_meta_name("protocol = 1;"), None);
    }

    #[test]
    fn finds_leftovers() {
        let (install, root) = temp_install("leftovers");
        std::fs::write(&install.manifest, ACF).unwrap();
        make_mod(&install, "111", 10);
        make_mod(&install, "222", 10); // installed, recorded under another account
        make_mod(&install, "333", 2000); // no record
        make_mod(&install, "444", 10); // subscribed, not installed yet
        make_mod(&install, "777", 10); // another account's, not installed
        make_mod(&install, "not-a-mod", 10);
        std::fs::write(install.workshop.join("555"), "a file, not a folder").unwrap();

        let inventory = scan(&install, Some(88216642));
        let mut installed: Vec<u64> = inventory.installed.iter().map(|f| f.id).collect();
        installed.sort();
        assert_eq!(installed, [111, 222, 444]);
        let a = inventory.get(111).unwrap();
        assert_eq!(a.size, 527656, "Steam's size for an installed mod");
        assert_eq!(a.name.as_deref(), Some("Mod 111"));
        assert_eq!(a.time_updated, 1771519119);
        assert_eq!(a.other_account, None);
        // Steam keeps one subscriber per mod, so this may be ours too: it's installed, marked.
        assert_eq!(inventory.get(222).unwrap().other_account, Some(5));
        let mut leftovers: Vec<(u64, Leftover)> = inventory
            .leftovers
            .iter()
            .map(|f| (f.id, f.leftover.unwrap()))
            .collect();
        leftovers.sort_by_key(|l| l.0);
        assert_eq!(
            leftovers,
            [(333, Leftover::NoRecord), (777, Leftover::OtherAccount(5))]
        );
        let c = inventory.get(333).unwrap();
        assert!(c.size >= 2000, "a leftover's size is counted");

        // Without knowing the account, another account's mods aren't leftovers or marked.
        let unknown = scan(&install, None);
        assert!(unknown.leftovers.iter().all(|f| f.id == 333));
        assert!(unknown.installed.iter().all(|f| f.other_account.is_none()));
        // Without Steam's records, nothing is.
        std::fs::remove_file(&install.manifest).unwrap();
        let blind = scan(&install, Some(88216642));
        assert!(blind.no_records && blind.leftovers.is_empty());
        assert_eq!(blind.installed.len(), 5);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn counts_who_uses_what() {
        let used = used_by(&[vec![1, 2, 2], vec![2, 3], vec![], vec![0, 1]]);
        assert_eq!(used[&1], [0, 3]);
        assert_eq!(used[&2], [0, 1], "each server once");
        assert_eq!(used[&3], [1]);
        assert!(!used.contains_key(&0), "not Workshop mods");
        assert!(!used.contains_key(&4));
    }

    #[test]
    fn deletes_only_safe_folders() {
        let (install, root) = temp_install("delete");
        let ws = &install.workshop;
        let good = make_mod(&install, "333", 10);
        assert_eq!(
            checked_folder(ws, &good).unwrap(),
            good.canonicalize().unwrap()
        );

        // Not digits, or not directly inside.
        std::fs::create_dir_all(ws.join("abc")).unwrap();
        assert!(checked_folder(ws, &ws.join("abc")).is_err());
        assert!(checked_folder(ws, &ws.join("333/addons")).is_err());
        assert!(checked_folder(ws, &ws.join("..")).is_err());
        assert!(checked_folder(ws, &ws.join("333/..")).is_err());
        let outside = root.join("elsewhere/444");
        std::fs::create_dir_all(&outside).unwrap();
        assert!(
            checked_folder(ws, &outside).is_err(),
            "outside the Workshop folder"
        );
        // A folder that isn't DayZ's Workshop folder.
        assert!(checked_folder(&root.join("elsewhere"), &outside).is_err());
        // Missing, or a file.
        assert!(checked_folder(ws, &ws.join("999")).is_err());
        std::fs::write(ws.join("555"), "x").unwrap();
        assert!(checked_folder(ws, &ws.join("555")).is_err());

        #[cfg(unix)]
        {
            // A link to a folder elsewhere is refused, and what it points at survives.
            std::os::unix::fs::symlink(&outside, ws.join("666")).unwrap();
            assert!(checked_folder(ws, &ws.join("666")).is_err());
            // A Workshop folder that is itself a link (a library moved to another drive) is
            // fine: the folder is checked where it really is.
            let linked = root.join("linked/content/221100");
            std::fs::create_dir_all(linked.parent().unwrap()).unwrap();
            std::os::unix::fs::symlink(root.join("elsewhere"), &linked).unwrap();
            assert_eq!(
                checked_folder(&linked, &linked.join("444")).unwrap(),
                outside.canonicalize().unwrap()
            );
        }

        // Deleting goes by Steam's records: an installed mod is refused, a leftover goes, and
        // links inside it are removed without touching what they point at.
        std::fs::write(&install.manifest, ACF).unwrap();
        let installed = make_mod(&install, "111", 10);
        assert!(delete_leftover_for(&install, 111, Some(88216642)).is_err());
        assert!(installed.is_dir());
        // Installed, though recorded under another account, or with the account unknown.
        let theirs = make_mod(&install, "222", 10);
        assert!(delete_leftover_for(&install, 222, Some(88216642)).is_err());
        let pending = make_mod(&install, "777", 10);
        assert!(delete_leftover_for(&install, 777, None).is_err());
        assert!(theirs.is_dir() && pending.is_dir());
        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside, good.join("inner-link")).unwrap();
        let freed = delete_leftover_for(&install, 333, Some(88216642)).unwrap();
        assert!(freed >= 10);
        assert!(!good.exists());
        assert!(outside.is_dir());
        #[cfg(unix)]
        {
            // A leftover that's a link is refused.
            assert!(delete_leftover_for(&install, 666, Some(88216642)).is_err());
            assert!(outside.is_dir());
        }
        std::fs::remove_dir_all(root).unwrap();
    }
}
