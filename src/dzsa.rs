//! The server list from DZSA Launcher's public API, which most third-party DayZ launchers use. It
//! lists every server DZSA knows (about 10,000) with each one's mods. There's no documentation or
//! published limit, so the list is cached on disk and only fetched again when it's old or the
//! user asks, and entries are read leniently.

use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::Path;
use std::time::SystemTime;

use crate::http;
use crate::server::{ModRef, Server, clean_name};

const LIST_URL: &str = "https://dayzsalauncher.com/api/v1/launcher/servers/dayz";
const CACHE_FILE: &str = "dzsa-servers.json";
/// The list is about 18 MB uncompressed.
const MAX_BODY: u64 = 128 * 1024 * 1024;

/// The cached list and when it was fetched, if there is one.
pub fn load_cache(dir: &Path) -> Option<(Vec<Server>, SystemTime)> {
    let path = dir.join(CACHE_FILE);
    let fetched = std::fs::metadata(&path).ok()?.modified().ok()?;
    let body = std::fs::read(&path).ok()?;
    parse(&body).ok().map(|servers| (servers, fetched))
}

/// Downloads the list (compressed on the wire) and caches it.
pub fn fetch(dir: &Path) -> Result<Vec<Server>> {
    let body = http::agent()
        .get(LIST_URL)
        .call()
        .context("couldn't reach DZSA")?
        .body_mut()
        .with_config()
        .limit(MAX_BODY)
        .read_to_vec()
        .context("couldn't download the DZSA list")?;
    let servers = parse(&body)?;
    std::fs::create_dir_all(dir).ok();
    let tmp = dir.join(format!("{CACHE_FILE}.tmp"));
    if std::fs::write(&tmp, &body).is_ok() {
        let _ = std::fs::rename(&tmp, dir.join(CACHE_FILE));
    }
    Ok(servers)
}

#[derive(Deserialize)]
struct Response {
    #[serde(default)]
    result: Vec<serde_json::Value>,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct Entry {
    name: String,
    /// The server's own name when DZSA shows a sponsor's custom one in `name`.
    #[serde(deserialize_with = "lenient")]
    original_name: Option<String>,
    map: String,
    #[serde(deserialize_with = "lenient")]
    players: Option<u32>,
    #[serde(deserialize_with = "lenient")]
    max_players: Option<u32>,
    #[serde(deserialize_with = "lenient")]
    game_port: Option<u16>,
    endpoint: Endpoint,
    mods: Vec<Mod>,
    #[serde(deserialize_with = "lenient")]
    password: Option<bool>,
    #[serde(deserialize_with = "lenient")]
    battl_eye: Option<bool>,
    #[serde(deserialize_with = "lenient")]
    first_person_only: Option<bool>,
    #[serde(deserialize_with = "lenient")]
    time: Option<String>,
    #[serde(deserialize_with = "lenient")]
    version: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Endpoint {
    ip: String,
    /// The query port.
    #[serde(deserialize_with = "lenient")]
    port: Option<u16>,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct Mod {
    name: String,
    #[serde(deserialize_with = "lenient")]
    steam_workshop_id: Option<u64>,
}

/// Any value of the wrong type reads as missing, rather than failing the whole entry.
fn lenient<'de, D, T>(d: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::de::DeserializeOwned,
{
    let value = serde_json::Value::deserialize(d)?;
    Ok(serde_json::from_value(value).ok())
}

fn parse(body: &[u8]) -> Result<Vec<Server>> {
    let response: Response =
        serde_json::from_slice(body).context("DZSA sent an unreadable list")?;
    // One entry at a time, so a single odd one doesn't lose the whole list.
    Ok(response
        .result
        .into_iter()
        .filter_map(|v| serde_json::from_value::<Entry>(v).ok())
        .filter_map(convert)
        .collect())
}

fn convert(e: Entry) -> Option<Server> {
    let query_port = e.endpoint.port?;
    if e.endpoint.ip.is_empty() {
        return None;
    }
    let mods: Vec<ModRef> = e
        .mods
        .into_iter()
        .map(|m| ModRef {
            id: m.steam_workshop_id.unwrap_or(0),
            name: clean_name(&m.name),
        })
        .collect();
    Some(Server {
        name: clean_name(e.original_name.as_deref().unwrap_or(&e.name)),
        map: e.map.to_lowercase(),
        ip: e.endpoint.ip,
        game_port: e.game_port.unwrap_or(2302),
        query_port,
        players: e.players.unwrap_or(0),
        max_players: e.max_players.unwrap_or(0),
        time: e.time.filter(|t| !t.is_empty()),
        version: e.version.unwrap_or_default(),
        password: e.password.unwrap_or(false),
        first_person: e.first_person_only.unwrap_or(false),
        battleye: e.battl_eye.unwrap_or(true),
        queue: None,
        modded: !mods.is_empty(),
        mods,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_entries_leniently() {
        let body = br#"{"status":0,"result":[
            {"name":"Sponsor Name","originalName":"Real Name","map":"Namalsk","players":5,
             "maxPlayers":60,"gamePort":2402,"endpoint":{"ip":"203.0.113.9","port":27016},
             "mods":[{"name":"CF","steamWorkshopId":1559212036}],"password":false,
             "battlEye":true,"firstPersonOnly":true,"time":"12:30","version":"1.28.0",
             "timeAcceleration":null,"sponsor":true},
            {"name":"no endpoint"},
            {"name":"odd types","players":"many","endpoint":{"ip":"203.0.113.10","port":27016}},
            {"name":"Vanilla","map":"chernarusplus","endpoint":{"ip":"203.0.113.11","port":2303}}
        ]}"#;
        let servers = parse(body).unwrap();
        assert_eq!(servers.len(), 3);
        let s = &servers[0];
        assert_eq!(s.name, "Real Name");
        assert_eq!(s.map, "namalsk");
        assert_eq!(s.addr(), "203.0.113.9:27016");
        assert_eq!(s.game_port, 2402);
        assert!(s.first_person && s.modded);
        assert_eq!(s.mods[0].id, 1559212036);
        assert_eq!(servers[1].players, 0, "unreadable count reads as 0");
        assert!(!servers[2].modded);
    }
}
