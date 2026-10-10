//! Steam Web API calls: the server list (needs the user's own key), Workshop item details (no
//! key), and subscribing to an item (key).
//!
//! The key goes in request URLs and forms, so it's scrubbed from every error before it can reach
//! the screen or a log.

use anyhow::{Result, anyhow, bail};
use serde::Deserialize;
use std::collections::{HashMap, HashSet};

use crate::a2s::parse_tags;
use crate::http;
use crate::server::{Server, clean_name};
use crate::steam::DAYZ_APP;

const SERVER_LIST: &str = "https://api.steampowered.com/IGameServersService/GetServerList/v1/";
const FILE_DETAILS: &str =
    "https://api.steampowered.com/ISteamRemoteStorage/GetPublishedFileDetails/v1/";
const SUBSCRIBE: &str = "https://api.steampowered.com/IPublishedFileService/Subscribe/v1/";

/// Steam returns at most this many servers per call (undocumented; DayZ has more), so the list is
/// fetched in slices.
const LIMIT: usize = 10_000;

/// Worlds big enough to get their own slice when a slice is still too large.
const BIG_MAPS: &[&str] = &["chernarusplus", "enoch", "sakhal", "deerisle", "namalsk"];

/// Removes the key from an error message.
fn scrub(message: String, key: &str) -> String {
    if key.is_empty() {
        message
    } else {
        message.replace(key, "<key>")
    }
}

fn err(e: impl std::fmt::Display, key: &str) -> anyhow::Error {
    anyhow!(scrub(e.to_string(), key))
}

#[derive(Deserialize)]
struct ListResponse {
    response: ListBody,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct ListBody {
    servers: Vec<ListServer>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct ListServer {
    /// `ip:query_port`.
    addr: String,
    gameport: u16,
    name: String,
    map: String,
    players: u32,
    max_players: u32,
    version: String,
    gametype: String,
}

fn list_slice(key: &str, filter: &str) -> Result<Vec<ListServer>> {
    let limit = LIMIT.to_string();
    let response: ListResponse = http::agent()
        .get(SERVER_LIST)
        .query("key", key)
        .query("filter", filter)
        .query("limit", &limit)
        .call()
        .map_err(|e| err(e, key))?
        .body_mut()
        .with_config()
        .limit(64 * 1024 * 1024)
        .read_json()
        .map_err(|e| err(e, key))?;
    Ok(response.response.servers)
}

/// Splits a filter into slices that don't overlap: first by empty or not, then by the big maps.
fn splits(level: usize) -> Option<(String, String)> {
    match level {
        0 => Some((r"\noplayers\1".into(), r"\empty\1".into())),
        n => BIG_MAPS
            .get(n - 1)
            .map(|map| (format!(r"\map\{map}"), format!(r"\nor\1\map\{map}"))),
    }
}

/// Fetches one slice, splitting it further while it's at Steam's limit.
fn collect(key: &str, filter: String, level: usize, out: &mut Vec<ListServer>) -> Result<()> {
    let slice = list_slice(key, &filter)?;
    if slice.len() < LIMIT - 50 {
        out.extend(slice);
        return Ok(());
    }
    match splits(level) {
        Some((a, b)) => {
            collect(key, format!("{filter}{a}"), level + 1, out)?;
            collect(key, format!("{filter}{b}"), level + 1, out)
        }
        None => {
            log::warn!("a server list slice hit Steam's limit; some servers may be missing");
            out.extend(slice);
            Ok(())
        }
    }
}

/// Every DayZ server Steam knows, in about ten calls. The whole list is always over the limit,
/// so the empty and non-empty halves are fetched side by side from the start.
pub fn server_list(key: &str) -> Result<Vec<Server>> {
    let base = format!(r"\appid\{DAYZ_APP}");
    let (a, b) = splits(0).expect("a first split");
    let halves = std::thread::scope(|s| {
        [a, b]
            .map(|half| {
                let filter = format!("{base}{half}");
                s.spawn(move || {
                    let mut servers = Vec::new();
                    collect(key, filter, 1, &mut servers).map(|_| servers)
                })
            })
            .map(|h| h.join().map_err(|_| anyhow!("server list thread failed")))
    });
    let mut seen = HashSet::new();
    let mut servers = Vec::new();
    for half in halves {
        servers.extend(
            half??
                .into_iter()
                .filter(|s| seen.insert(s.addr.clone()))
                .filter_map(convert),
        );
    }
    Ok(servers)
}

fn convert(s: ListServer) -> Option<Server> {
    let (ip, port) = s.addr.rsplit_once(':')?;
    let tags = parse_tags(&s.gametype);
    Some(Server {
        name: clean_name(&s.name),
        map: s.map.to_lowercase(),
        ip: ip.to_string(),
        game_port: s.gameport,
        query_port: port.parse().ok()?,
        players: s.players,
        max_players: s.max_players,
        time: tags.time,
        version: s.version,
        // Not in Steam's list; A2S_INFO fills it in.
        password: false,
        first_person: tags.first_person,
        battleye: tags.battleye,
        queue: tags.queue,
        mods: Vec::new(),
        modded: tags.modded,
    })
}

/// Checks a key with a tiny request.
pub fn test_key(key: &str) -> Result<()> {
    let key = key.trim();
    if key.len() != 32 || !key.bytes().all(|b| b.is_ascii_hexdigit()) {
        bail!("a Steam Web API key is 32 letters and digits (0-9, A-F)");
    }
    let response: ListResponse = http::agent()
        .get(SERVER_LIST)
        .query("key", key)
        .query("filter", format!(r"\appid\{DAYZ_APP}"))
        .query("limit", "1")
        .call()
        .map_err(|e| match e {
            ureq::Error::StatusCode(401 | 403) => anyhow!("Steam didn't accept this key"),
            other => err(other, key),
        })?
        .body_mut()
        .read_json()
        .map_err(|e| err(e, key))?;
    let _ = response.response.servers;
    Ok(())
}

/// What the browser needs from a Workshop item's public details.
#[derive(Debug, Clone, Default)]
pub struct FileDetails {
    /// Unix time of the last update.
    pub time_updated: u64,
    /// The download's size in bytes, when Steam says.
    pub size: Option<u64>,
}

#[derive(Deserialize)]
struct DetailsResponse {
    response: DetailsBody,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct DetailsBody {
    publishedfiledetails: Vec<DetailsItem>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct DetailsItem {
    publishedfileid: String,
    result: i32,
    time_updated: u64,
    #[serde(deserialize_with = "lenient_size")]
    file_size: Option<u64>,
}

/// Steam sends `file_size` as a string of digits (`"123456"`), though a number is accepted too.
/// Anything else, or zero, counts as unknown rather than failing the whole response.
fn lenient_size<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<u64>, D::Error> {
    Ok(match serde_json::Value::deserialize(d)? {
        serde_json::Value::Number(n) => n.as_u64(),
        serde_json::Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
    .filter(|&size| size > 0))
}

/// Details for Workshop items (no key needed). Items Steam doesn't return are left out.
pub fn file_details(ids: &[u64]) -> Result<HashMap<u64, FileDetails>> {
    let mut out = HashMap::new();
    for batch in ids.chunks(100) {
        let mut form: Vec<(String, String)> = vec![("itemcount".into(), batch.len().to_string())];
        for (i, id) in batch.iter().enumerate() {
            form.push((format!("publishedfileids[{i}]"), id.to_string()));
        }
        let response: DetailsResponse = http::agent()
            .post(FILE_DETAILS)
            .send_form(form.iter().map(|(k, v)| (k.as_str(), v.as_str())))?
            .body_mut()
            .read_json()?;
        for item in response.response.publishedfiledetails {
            let Ok(id) = item.publishedfileid.parse::<u64>() else {
                continue;
            };
            if item.result != 1 {
                continue;
            }
            out.insert(
                id,
                FileDetails {
                    time_updated: item.time_updated,
                    size: item.file_size,
                },
            );
        }
    }
    Ok(out)
}

/// Subscribes the key's account to a Workshop item; the running Steam client then downloads it.
pub fn subscribe(key: &str, id: u64) -> Result<()> {
    http::agent()
        .post(SUBSCRIBE)
        .send_form([
            ("key", key),
            ("publishedfileid", &id.to_string()),
            ("list_type", "1"),
            ("appid", &DAYZ_APP.to_string()),
            ("notify_client", "1"),
        ])
        .map_err(|e| err(e, key))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scrubs_the_key() {
        let key = "0123456789ABCDEF0123456789ABCDEF";
        let message = format!("https://api.steampowered.com/x?key={key}&filter=1: timeout");
        assert!(!scrub(message, key).contains(key));
    }

    #[test]
    fn converts_steam_entries() {
        let s = convert(ListServer {
            addr: "203.0.113.4:27016".into(),
            gameport: 2302,
            name: "Name".into(),
            map: "ChernarusPlus".into(),
            players: 3,
            max_players: 60,
            version: "1.28".into(),
            gametype: "battleye,no3rd,mod,lqs0,08:15".into(),
        })
        .unwrap();
        assert_eq!(s.addr(), "203.0.113.4:27016");
        assert_eq!(s.map, "chernarusplus");
        assert!(s.first_person && s.modded && s.battleye);
        assert_eq!(s.time.as_deref(), Some("08:15"));
    }

    #[test]
    fn file_size_as_string_or_number() {
        let body = r#"{"response":{"result":1,"resultcount":4,"publishedfiledetails":[
            {"publishedfileid":"1559212036","result":1,"file_size":"3240000000","time_updated":1700000000},
            {"publishedfileid":"1564026768","result":1,"file_size":52000000},
            {"publishedfileid":"2116157322","result":1,"file_size":"lots"},
            {"publishedfileid":"2545327648","result":9}
        ]}}"#;
        let items = serde_json::from_str::<DetailsResponse>(body)
            .unwrap()
            .response
            .publishedfiledetails;
        let sizes: Vec<Option<u64>> = items.iter().map(|i| i.file_size).collect();
        assert_eq!(sizes, [Some(3_240_000_000), Some(52_000_000), None, None]);
        assert_eq!(items[0].time_updated, 1_700_000_000);
        assert_eq!(items[3].result, 9);
    }

    #[test]
    fn slices_dont_overlap() {
        assert_eq!(splits(0).unwrap().0, r"\noplayers\1");
        assert_eq!(splits(1).unwrap().1, r"\nor\1\map\chernarusplus");
        assert!(splits(BIG_MAPS.len() + 1).is_none());
    }
}
