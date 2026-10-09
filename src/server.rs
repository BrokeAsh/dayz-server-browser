//! A server as the browser shows it, whichever list it came from, and the filters and sorting
//! over the list.

use serde::{Deserialize, Serialize};
use std::cmp::Ordering;

#[derive(Debug, Clone, Default)]
pub struct Server {
    pub name: String,
    /// The world name in lower case (`chernarusplus`, `deerisle`), as servers report it.
    pub map: String,
    pub ip: String,
    pub game_port: u16,
    pub query_port: u16,
    pub players: u32,
    pub max_players: u32,
    /// In-game time, `HH:MM`.
    pub time: Option<String>,
    pub version: String,
    pub password: bool,
    pub first_person: bool,
    pub battleye: bool,
    /// Players waiting to get in.
    pub queue: Option<u32>,
    /// Mods in load order, when the list includes them (DZSA's does, Steam's doesn't).
    pub mods: Vec<ModRef>,
    /// Whether the server runs mods; true when `mods` isn't empty or the server says so.
    pub modded: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ModRef {
    /// The Steam Workshop item id; 0 for a mod that isn't on the Workshop.
    pub id: u64,
    pub name: String,
}

impl Server {
    /// `ip:query_port`, which identifies a server in both lists.
    pub fn addr(&self) -> String {
        format!("{}:{}", self.ip, self.query_port)
    }

    pub fn is_full(&self) -> bool {
        self.max_players > 0 && self.players >= self.max_players
    }
}

/// Server names are free text; drop control characters and trim.
pub fn clean_name(name: &str) -> String {
    let cleaned: String = name.chars().filter(|c| !c.is_control()).collect();
    cleaned.trim().to_string()
}

/// Display name for a world: common ones by their in-game name, others as written.
pub fn map_title(map: &str) -> String {
    match map.to_ascii_lowercase().as_str() {
        "chernarusplus" => "Chernarus".into(),
        "enoch" => "Livonia".into(),
        "sakhal" => "Sakhal".into(),
        "namalsk" => "Namalsk".into(),
        "deerisle" => "Deer Isle".into(),
        "" => "?".into(),
        _ => {
            let mut chars = map.chars();
            chars
                .next()
                .map(|c| c.to_uppercase().chain(chars).collect())
                .unwrap_or_default()
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Perspective {
    #[default]
    Any,
    First,
    Third,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ModFilter {
    #[default]
    Any,
    Vanilla,
    Modded,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Filters {
    pub search: String,
    /// Lower-case world name, or empty for any.
    pub map: String,
    pub hide_empty: bool,
    pub hide_full: bool,
    pub hide_password: bool,
    pub perspective: Perspective,
    pub mods: ModFilter,
    /// Hide servers slower than this (ms); 0 for no limit. Servers not pinged yet stay visible.
    pub max_ping: u32,
    pub favorites_only: bool,
}

impl Filters {
    pub fn matches(&self, s: &Server, ping: Option<u32>, favorite: bool) -> bool {
        if self.favorites_only && !favorite {
            return false;
        }
        if !self.map.is_empty() && !s.map.eq_ignore_ascii_case(&self.map) {
            return false;
        }
        if (self.hide_empty && s.players == 0)
            || (self.hide_full && s.is_full())
            || (self.hide_password && s.password)
        {
            return false;
        }
        match self.perspective {
            Perspective::First if !s.first_person => return false,
            Perspective::Third if s.first_person => return false,
            _ => {}
        }
        match self.mods {
            ModFilter::Vanilla if s.modded => return false,
            ModFilter::Modded if !s.modded => return false,
            _ => {}
        }
        if self.max_ping > 0 && ping.is_some_and(|p| p > self.max_ping) {
            return false;
        }
        let search = self.search.trim();
        if !search.is_empty() {
            let needle = search.to_lowercase();
            let hit = s.name.to_lowercase().contains(&needle)
                || s.map.to_lowercase().contains(&needle)
                || s.addr().contains(&needle)
                || s.mods
                    .iter()
                    .any(|m| m.name.to_lowercase().contains(&needle));
            if !hit {
                return false;
            }
        }
        true
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SortBy {
    Favorite,
    Name,
    Map,
    Players,
    Time,
    Ping,
    Mods,
}

/// Compares two servers by a column; unknown pings sort after known ones either way.
pub fn compare(
    by: SortBy,
    a: &Server,
    b: &Server,
    ping: impl Fn(&Server) -> Option<u32>,
    favorite: impl Fn(&Server) -> bool,
) -> Ordering {
    match by {
        SortBy::Favorite => favorite(b).cmp(&favorite(a)),
        SortBy::Name => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
        SortBy::Map => map_title(&a.map).cmp(&map_title(&b.map)),
        SortBy::Players => a.players.cmp(&b.players),
        SortBy::Time => a.time.cmp(&b.time),
        SortBy::Ping => match (ping(a), ping(b)) {
            (Some(x), Some(y)) => x.cmp(&y),
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (None, None) => Ordering::Equal,
        },
        SortBy::Mods => a.mods.len().cmp(&b.mods.len()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn server() -> Server {
        Server {
            name: "Test | Deer Isle PvE".into(),
            map: "deerisle".into(),
            ip: "203.0.113.5".into(),
            game_port: 2302,
            query_port: 27016,
            players: 10,
            max_players: 60,
            mods: vec![ModRef {
                id: 1559212036,
                name: "Community Framework".into(),
            }],
            modded: true,
            ..Default::default()
        }
    }

    #[test]
    fn filters() {
        let s = server();
        let mut f = Filters::default();
        assert!(f.matches(&s, None, false));
        f.search = "framework".into();
        assert!(f.matches(&s, None, false));
        f.search = "namalsk".into();
        assert!(!f.matches(&s, None, false));
        f = Filters {
            mods: ModFilter::Vanilla,
            ..Default::default()
        };
        assert!(!f.matches(&s, None, false));
        f = Filters {
            max_ping: 50,
            ..Default::default()
        };
        assert!(f.matches(&s, None, false), "not pinged yet: keep it");
        assert!(!f.matches(&s, Some(80), false));
        f = Filters {
            favorites_only: true,
            ..Default::default()
        };
        assert!(!f.matches(&s, None, false));
        assert!(f.matches(&s, None, true));
    }

    #[test]
    fn names() {
        assert_eq!(clean_name(" A\u{1}B\n "), "AB");
        assert_eq!(map_title("enoch"), "Livonia");
        assert_eq!(map_title("takistanplus"), "Takistanplus");
    }
}
