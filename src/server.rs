//! A server as the browser shows it, whichever list it came from, and the filters and sorting
//! over the list.

use serde::{Deserialize, Serialize};
use std::cmp::Ordering;

use crate::settings::lenient;

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
    /// One of the game's own servers (DZSA's `public` shard; elsewhere, by the server's tags).
    pub official: bool,
    /// How many times faster than real time the in-game day and night run, when known.
    pub day_speed: Option<f32>,
    pub night_speed: Option<f32>,
    /// Where the server is, as an upper-case ISO 3166 code (`DE`), once looked up.
    pub country: Option<String>,
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

/// What the map filter compares, so names for the same world (`enoch` and `Livonia`,
/// `deerisle` and `DeerIsle`) count as one map.
pub fn map_key(map: &str) -> String {
    map_title(map).to_lowercase()
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

/// The game's own servers, or everyone else's.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Kind {
    #[default]
    Any,
    Official,
    Community,
}

/// Day is 06:00 to 17:59 in-game, night the rest.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Daytime {
    #[default]
    Any,
    Day,
    Night,
}

/// Every field reads leniently: a value of the wrong kind (such as a choice a newer version
/// saved) gives that filter's default rather than losing the whole state.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Filters {
    #[serde(deserialize_with = "lenient")]
    pub search: String,
    /// Maps by [`map_key`]; empty for any.
    #[serde(alias = "map", deserialize_with = "one_or_many")]
    pub maps: Vec<String>,
    #[serde(deserialize_with = "lenient")]
    pub hide_empty: bool,
    #[serde(deserialize_with = "lenient")]
    pub hide_full: bool,
    #[serde(deserialize_with = "lenient")]
    pub hide_password: bool,
    #[serde(deserialize_with = "lenient")]
    pub perspective: Perspective,
    #[serde(deserialize_with = "lenient")]
    pub mods: ModFilter,
    /// Hide servers slower than this (ms); 0 for no limit. Servers not pinged yet stay visible.
    #[serde(deserialize_with = "lenient")]
    pub max_ping: u32,
    #[serde(deserialize_with = "lenient")]
    pub favorites_only: bool,
    /// Limits on players online and on server size (slots); 0 for no limit.
    #[serde(deserialize_with = "lenient")]
    pub min_players: u32,
    #[serde(deserialize_with = "lenient")]
    pub max_players: u32,
    #[serde(deserialize_with = "lenient")]
    pub min_slots: u32,
    #[serde(deserialize_with = "lenient")]
    pub max_slots: u32,
    /// Hide servers known to run a different version of the game than the one installed.
    #[serde(deserialize_with = "lenient")]
    pub hide_other_versions: bool,
    #[serde(deserialize_with = "lenient")]
    pub kind: Kind,
    #[serde(deserialize_with = "lenient")]
    pub battleye_only: bool,
    /// By the server's in-game time; servers whose time isn't known are hidden while it's set.
    #[serde(deserialize_with = "lenient")]
    pub daytime: Daytime,
    /// Hide servers less than 30% full.
    #[serde(deserialize_with = "lenient")]
    pub hide_low_population: bool,
    /// Countries by upper-case ISO code; empty for any. Servers whose country isn't known yet
    /// stay visible.
    #[serde(deserialize_with = "country_codes")]
    pub countries: Vec<String>,
    /// Limits on how many times faster than real time the in-game day runs; 0 for no limit.
    #[serde(deserialize_with = "lenient")]
    pub min_speed: u32,
    #[serde(deserialize_with = "lenient")]
    pub max_speed: u32,
    /// Show the servers that look fake or are listed twice, which are hidden by default (except
    /// the user's own: see [`Seen::suspect`]).
    #[serde(deserialize_with = "lenient")]
    pub show_suspect: bool,
}

/// What's known about a server beyond the list: its latest answer to a ping, and the user's own
/// marks. The answer is newer than the list, so it wins where both say something.
#[derive(Debug, Clone, Default)]
pub struct Seen {
    pub ping: Option<u32>,
    /// Players and slots.
    pub players: Option<(u32, u32)>,
    pub queue: Option<u32>,
    /// In-game time, `HH:MM`.
    pub time: Option<String>,
    /// Needs a password (the Steam list doesn't say; the server's answer does).
    pub locked: bool,
    pub favorite: bool,
    /// Whether it runs the installed game's version; `None` if either is unknown.
    pub same_version: Option<bool>,
    /// The user's note on it; empty if none.
    pub note: String,
    /// From the answer's tags: BattlEye, and how fast time runs by day.
    pub battleye: Option<bool>,
    pub day_speed: Option<f32>,
    /// Looks fake or is a second listing (see `suspect`), and isn't a server the user keeps
    /// (a favorite, a recent one, or one added by address), which always show.
    pub suspect: bool,
}

/// Reads country codes, keeping the two-letter ones, upper case and once each.
fn country_codes<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<String>, D::Error> {
    let codes: Vec<serde_json::Value> = lenient(d)?;
    let mut kept: Vec<String> = Vec::new();
    for code in codes.iter().filter_map(|c| c.as_str()) {
        let code = code.trim().to_ascii_uppercase();
        if code.len() == 2 && code.bytes().all(|b| b.is_ascii_alphabetic()) && !kept.contains(&code)
        {
            kept.push(code);
        }
    }
    Ok(kept)
}

/// Whether an in-game time (`HH:MM`) is day (06:00 to 17:59); `None` if it isn't a time.
pub fn is_day(time: &str) -> Option<bool> {
    let (hours, minutes) = time.trim().split_once(':')?;
    let hours: u32 = hours.parse().ok()?;
    let minutes: u32 = minutes.parse().ok()?;
    (hours < 24 && minutes < 60).then_some((6..18).contains(&hours))
}

/// Whether a server is less than 30% full. Servers without a slot count can't say.
pub fn low_population(players: u32, slots: u32) -> bool {
    slots > 0 && u64::from(players) * 10 < u64::from(slots) * 3
}

impl Filters {
    /// The filters in the "More filters" section.
    fn matches_more(&self, s: &Server, seen: &Seen) -> bool {
        let (players, slots) = seen.players.unwrap_or((s.players, s.max_players));
        match self.kind {
            Kind::Official if !s.official => return false,
            Kind::Community if s.official => return false,
            _ => {}
        }
        if (self.battleye_only && !seen.battleye.unwrap_or(s.battleye))
            || (self.hide_low_population && low_population(players, slots))
            || (!self.show_suspect && seen.suspect)
        {
            return false;
        }
        if self.daytime != Daytime::Any {
            let day = seen.time.as_deref().or(s.time.as_deref()).and_then(is_day);
            if day != Some(self.daytime == Daytime::Day) {
                return false;
            }
        }
        if !self.countries.is_empty()
            && s.country
                .as_ref()
                .is_some_and(|c| !self.countries.contains(c))
        {
            return false;
        }
        if self.min_speed > 0 || self.max_speed > 0 {
            // Within a whisker, so 4.000001 counts as 4.
            let Some(speed) = seen.day_speed.or(s.day_speed) else {
                return false;
            };
            if speed < self.min_speed as f32 - 0.01
                || (self.max_speed > 0 && speed > self.max_speed as f32 + 0.01)
            {
                return false;
            }
        }
        true
    }
}

/// Whether a search (in lower case) matches a server's country: its code exactly, or (from three
/// letters on) the start of its name or of a word in it. So "germ" and "kingdom" find Germany and
/// the United Kingdom, but "man" doesn't match Germany or Oman, nor "us" Belarus.
fn country_hit(s: &Server, needle: &str) -> bool {
    s.country.as_deref().is_some_and(|code| {
        code.eq_ignore_ascii_case(needle)
            || (needle.chars().count() >= 3
                && crate::geo::name(code).is_some_and(|n| name_starts(&n.to_lowercase(), needle)))
    })
}

/// Whether `needle` starts a country's name (in lower case) or one of its words, leaving out
/// the small ones ("and", "of", "the") so "and" doesn't find Trinidad and Tobago.
fn name_starts(name: &str, needle: &str) -> bool {
    const SMALL: &[&str] = &["and", "of", "the"];
    name.starts_with(needle)
        || name
            .split(|c: char| !c.is_alphanumeric())
            .filter(|word| !SMALL.contains(word))
            .any(|word| word.starts_with(needle))
}

/// Reads a list of maps, or the single world name (empty for any) that older versions saved.
/// Anything else reads as any map, and entries that aren't names are skipped.
fn one_or_many<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<String>, D::Error> {
    let maps = match serde_json::Value::deserialize(d)? {
        serde_json::Value::String(map) => vec![map],
        serde_json::Value::Array(maps) => maps
            .into_iter()
            .filter_map(|m| m.as_str().map(str::to_string))
            .collect(),
        _ => Vec::new(),
    };
    let mut keys: Vec<String> = Vec::new();
    for key in maps.iter().filter(|m| !m.is_empty()).map(|m| map_key(m)) {
        if !keys.contains(&key) {
            keys.push(key);
        }
    }
    Ok(keys)
}

impl Filters {
    pub fn matches(&self, s: &Server, seen: &Seen) -> bool {
        let (players, slots) = seen.players.unwrap_or((s.players, s.max_players));
        if (self.favorites_only && !seen.favorite)
            || (self.hide_other_versions && seen.same_version == Some(false))
        {
            return false;
        }
        if !self.maps.is_empty() && !self.maps.contains(&map_key(&s.map)) {
            return false;
        }
        if (self.hide_empty && players == 0)
            || (self.hide_full && slots > 0 && players >= slots)
            || (self.hide_password && (s.password || seen.locked))
        {
            return false;
        }
        let within = |n: u32, min: u32, max: u32| n >= min && (max == 0 || n <= max);
        if !within(players, self.min_players, self.max_players)
            || !within(slots, self.min_slots, self.max_slots)
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
        if self.max_ping > 0 && seen.ping.is_some_and(|p| p > self.max_ping) {
            return false;
        }
        if !self.matches_more(s, seen) {
            return false;
        }
        let search = self.search.trim();
        if !search.is_empty() {
            let needle = search.to_lowercase();
            let hit = s.name.to_lowercase().contains(&needle)
                || s.map.to_lowercase().contains(&needle)
                // Either port: the query port identifies it here, the game port is what most
                // places show (and the bare IP matches both).
                || s.addr().contains(&needle)
                || format!("{}:{}", s.ip, s.game_port).contains(&needle)
                || s.mods
                    .iter()
                    .any(|m| m.name.to_lowercase().contains(&needle))
                || seen.note.to_lowercase().contains(&needle)
                || country_hit(s, &needle);
            if !hit {
                return false;
            }
        }
        true
    }
}

/// The list's sort column; busiest first by default.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum SortBy {
    Favorite,
    Name,
    Map,
    #[default]
    Players,
    Queue,
    Time,
    Ping,
    Mods,
    Country,
}

/// Compares two servers by a column, smallest first. Unknown values (pings, countries, times)
/// compare as greater, and [`one_unknown`] tells the caller to keep them last when it reverses
/// the order.
pub fn compare(by: SortBy, a: &Server, x: &Seen, b: &Server, y: &Seen) -> Ordering {
    let players = |s: &Server, seen: &Seen| seen.players.map_or(s.players, |(p, _)| p);
    let queue = |s: &Server, seen: &Seen| seen.queue.or(s.queue).unwrap_or(0);
    match by {
        SortBy::Favorite => y.favorite.cmp(&x.favorite),
        SortBy::Name => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
        SortBy::Map => map_title(&a.map).cmp(&map_title(&b.map)),
        SortBy::Players => players(a, x).cmp(&players(b, y)),
        SortBy::Queue => queue(a, x).cmp(&queue(b, y)),
        // Servers whose time isn't known after the rest.
        SortBy::Time => {
            let (ta, tb) = (
                x.time.as_deref().or(a.time.as_deref()),
                y.time.as_deref().or(b.time.as_deref()),
            );
            (ta.is_none(), ta).cmp(&(tb.is_none(), tb))
        }
        SortBy::Ping => match (x.ping, y.ping) {
            (Some(x), Some(y)) => x.cmp(&y),
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (None, None) => Ordering::Equal,
        },
        // Servers whose mods aren't listed (Steam's list) still sort after vanilla ones.
        SortBy::Mods => (a.modded, a.mods.len()).cmp(&(b.modded, b.mods.len())),
        // By code, A to Z, with servers whose country isn't known after the rest.
        SortBy::Country => {
            (a.country.is_none(), &a.country).cmp(&(b.country.is_none(), &b.country))
        }
    }
}

/// Whether one of two servers has the column's value and the other doesn't (no ping, no known
/// country, no in-game time): then [`compare`]'s order stays when sorting the other way too, so
/// servers that can't be placed are always last.
pub fn one_unknown(by: SortBy, a: &Server, x: &Seen, b: &Server, y: &Seen) -> bool {
    let time = |s: &Server, seen: &Seen| seen.time.is_some() || s.time.is_some();
    match by {
        SortBy::Ping => x.ping.is_some() != y.ping.is_some(),
        SortBy::Country => a.country.is_some() != b.country.is_some(),
        SortBy::Time => time(a, x) != time(b, y),
        _ => false,
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
        assert!(f.matches(&s, &Seen::default()));
        f.search = "framework".into();
        assert!(f.matches(&s, &Seen::default()));
        f.search = "namalsk".into();
        assert!(!f.matches(&s, &Seen::default()));
        f.search = "BASE".into();
        assert!(!f.matches(&s, &Seen::default()));
        let noted = Seen {
            note: "Our base is north of Sinystok".into(),
            ..Default::default()
        };
        assert!(f.matches(&s, &noted));
        for (search, keep) in [
            ("203.0.113.5", true),
            ("203.0.113.5:27016", true),
            ("203.0.113.5:2302", true),
            (" 203.0.113.5:2302 ", true),
            ("203.0.113.5:2402", false),
            ("203.0.113.6", false),
        ] {
            f.search = search.into();
            assert_eq!(f.matches(&s, &Seen::default()), keep, "{search}");
        }
        f = Filters {
            mods: ModFilter::Vanilla,
            ..Default::default()
        };
        assert!(!f.matches(&s, &Seen::default()));
        f = Filters {
            max_ping: 50,
            ..Default::default()
        };
        assert!(f.matches(&s, &Seen::default()), "not pinged yet: keep it");
        assert!(!f.matches(
            &s,
            &Seen {
                ping: Some(80),
                ..Default::default()
            }
        ));
        f = Filters {
            favorites_only: true,
            ..Default::default()
        };
        assert!(!f.matches(&s, &Seen::default()));
        assert!(f.matches(
            &s,
            &Seen {
                favorite: true,
                ..Default::default()
            }
        ));
        // 10 players on a 60-slot server.
        for (min_players, max_players, min_slots, max_slots, keep) in [
            (10, 10, 60, 60, true),
            (11, 0, 0, 0, false),
            (0, 9, 0, 0, false),
            (0, 0, 61, 0, false),
            (0, 0, 0, 59, false),
            (5, 20, 50, 100, true),
        ] {
            f = Filters {
                min_players,
                max_players,
                min_slots,
                max_slots,
                ..Default::default()
            };
            assert_eq!(f.matches(&s, &Seen::default()), keep, "{f:?}");
        }
        // The server's own answer wins over the list.
        f = Filters {
            min_players: 11,
            ..Default::default()
        };
        assert!(f.matches(
            &s,
            &Seen {
                players: Some((12, 60)),
                ..Default::default()
            }
        ));
        f = Filters {
            hide_full: true,
            ..Default::default()
        };
        assert!(!f.matches(
            &s,
            &Seen {
                players: Some((60, 60)),
                ..Default::default()
            }
        ));
        f = Filters {
            maps: vec!["chernarus".into(), "deer isle".into()],
            ..Default::default()
        };
        assert!(f.matches(&s, &Seen::default()));
        f.maps.remove(1);
        assert!(!f.matches(&s, &Seen::default()));
        f = Filters {
            hide_other_versions: true,
            ..Default::default()
        };
        assert!(f.matches(&s, &Seen::default()), "unknown version: keep it");
        let other = Seen {
            same_version: Some(false),
            ..Default::default()
        };
        assert!(!f.matches(&s, &other));
    }

    #[test]
    fn more_filters() {
        let mut s = server();
        s.time = Some("17:59".into());
        s.country = Some("DE".into());
        s.day_speed = Some(4.0);
        s.battleye = true;
        let none = Seen::default();
        let keeps = |f: &Filters, s: &Server, seen: &Seen| f.matches(s, seen);
        let f = Filters::default();
        assert!(keeps(&f, &s, &none));

        let kind = |kind| Filters {
            kind,
            ..Default::default()
        };
        assert!(keeps(&kind(Kind::Community), &s, &none));
        assert!(!keeps(&kind(Kind::Official), &s, &none));
        s.official = true;
        assert!(keeps(&kind(Kind::Official), &s, &none));
        assert!(!keeps(&kind(Kind::Community), &s, &none));

        // 17:59 is still day; the answer's time wins over the list's.
        let day = |daytime| Filters {
            daytime,
            ..Default::default()
        };
        assert!(keeps(&day(Daytime::Day), &s, &none));
        assert!(!keeps(&day(Daytime::Night), &s, &none));
        let night = Seen {
            time: Some("18:00".into()),
            ..Default::default()
        };
        assert!(keeps(&day(Daytime::Night), &s, &night));
        assert!(!keeps(&day(Daytime::Day), &s, &night));
        let mut no_time = s.clone();
        no_time.time = None;
        assert!(!keeps(&day(Daytime::Day), &no_time, &none), "unknown time");
        assert!(keeps(&day(Daytime::Any), &no_time, &none));

        let battleye = Filters {
            battleye_only: true,
            ..Default::default()
        };
        assert!(keeps(&battleye, &s, &none));
        let off = Seen {
            battleye: Some(false),
            ..Default::default()
        };
        assert!(!keeps(&battleye, &s, &off), "the answer's tags win");

        // 10 of 60 is under 30%; 18 of 60 isn't.
        let low = Filters {
            hide_low_population: true,
            ..Default::default()
        };
        assert!(!keeps(&low, &s, &none));
        let busier = Seen {
            players: Some((18, 60)),
            ..Default::default()
        };
        assert!(keeps(&low, &s, &busier));
        assert!(low_population(0, 10) && low_population(29, 100) && !low_population(30, 100));
        assert!(!low_population(0, 0), "no slot count: can't say");

        let countries = |codes: &[&str]| Filters {
            countries: codes.iter().map(|c| c.to_string()).collect(),
            ..Default::default()
        };
        assert!(keeps(&countries(&["FR", "DE"]), &s, &none));
        assert!(!keeps(&countries(&["FR"]), &s, &none));
        let mut unknown = s.clone();
        unknown.country = None;
        assert!(
            keeps(&countries(&["FR"]), &unknown, &none),
            "not known yet: keep it"
        );

        let speed = |min_speed, max_speed| Filters {
            min_speed,
            max_speed,
            ..Default::default()
        };
        assert!(keeps(&speed(4, 4), &s, &none));
        assert!(keeps(&speed(1, 0), &s, &none));
        assert!(!keeps(&speed(5, 0), &s, &none));
        assert!(!keeps(&speed(0, 3), &s, &none));
        let fast = Seen {
            day_speed: Some(12.0),
            ..Default::default()
        };
        assert!(keeps(&speed(10, 0), &s, &fast), "the answer's speed wins");
        let mut no_speed = s.clone();
        no_speed.day_speed = None;
        assert!(!keeps(&speed(1, 0), &no_speed, &none));

        let suspect = Seen {
            suspect: true,
            ..Default::default()
        };
        assert!(!keeps(&f, &s, &suspect), "hidden by default");
        let show = Filters {
            show_suspect: true,
            ..Default::default()
        };
        assert!(keeps(&show, &s, &suspect));
    }

    #[test]
    fn searches_countries() {
        let mut s = server();
        s.country = Some("DE".into());
        let search = |text: &str| Filters {
            search: text.into(),
            ..Default::default()
        };
        assert!(search("de").matches(&s, &Seen::default()));
        assert!(search("germ").matches(&s, &Seen::default()));
        assert!(search("Germany").matches(&s, &Seen::default()));
        assert!(!search("france").matches(&s, &Seen::default()));
        s.country = Some("BY".into());
        assert!(
            !search("us").matches(&s, &Seen::default()),
            "not a part of Belarus"
        );
        s.country = None;
        assert!(!search("germany").matches(&s, &Seen::default()));

        // Only the start of the name or of a word in it, so searching for part of a server's
        // name doesn't bring in whole countries.
        s.name = "Some server".into();
        for (code, text, hit) in [
            ("DE", "man", false),
            ("RO", "man", false),
            ("OM", "man", false),
            ("OM", "oman", true),
            ("IM", "man", true),
            ("RU", "rus", true),
            ("BY", "rus", false),
            ("CY", "rus", false),
            ("FI", "and", false),
            ("PL", "and", false),
            ("TT", "and", false),
            ("TT", "tobago", true),
            ("IS", "ice", true),
            ("GB", "kingdom", true),
            ("GB", "united k", true),
            ("US", "states", true),
            ("US", "us", true),
            ("BY", "us", false),
            ("GW", "bissau", true),
        ] {
            s.country = Some(code.into());
            assert_eq!(
                search(text).matches(&s, &Seen::default()),
                hit,
                "{text} in {code}"
            );
        }
    }

    #[test]
    fn days_and_nights() {
        for (time, day) in [
            ("06:00", Some(true)),
            ("17:59", Some(true)),
            ("18:00", Some(false)),
            ("05:59", Some(false)),
            ("00:00", Some(false)),
            ("12:30", Some(true)),
            ("24:00", None),
            ("12:60", None),
            ("noon", None),
            ("", None),
        ] {
            assert_eq!(is_day(time), day, "{time}");
        }
    }

    #[test]
    fn sorts_by_country() {
        let mut a = server();
        let mut b = server();
        a.country = Some("DE".into());
        b.country = Some("AT".into());
        let none = Seen::default();
        assert_eq!(
            compare(SortBy::Country, &a, &none, &b, &none),
            Ordering::Greater
        );
        b.country = None;
        assert_eq!(
            compare(SortBy::Country, &a, &none, &b, &none),
            Ordering::Less,
            "unknown after the rest"
        );
        // Which keeps them last when sorting the other way too.
        assert!(one_unknown(SortBy::Country, &a, &none, &b, &none));
        b.country = Some("AT".into());
        assert!(!one_unknown(SortBy::Country, &a, &none, &b, &none));
    }

    #[test]
    fn unknown_times_and_pings_sort_last() {
        let mut a = server();
        let b = server();
        a.time = Some("12:00".into());
        let none = Seen::default();
        assert_eq!(compare(SortBy::Time, &a, &none, &b, &none), Ordering::Less);
        assert!(one_unknown(SortBy::Time, &a, &none, &b, &none));
        // The answer's time counts too.
        let answered = Seen {
            time: Some("06:00".into()),
            ping: Some(40),
            ..Default::default()
        };
        assert_eq!(
            compare(SortBy::Time, &a, &none, &b, &answered),
            Ordering::Greater
        );
        assert!(!one_unknown(SortBy::Time, &a, &none, &b, &answered));
        assert!(one_unknown(SortBy::Ping, &a, &none, &b, &answered));
        assert!(!one_unknown(SortBy::Players, &a, &none, &b, &answered));
    }

    #[test]
    fn reads_old_map_filter() {
        let f: Filters = serde_json::from_str(r#"{"map":"enoch"}"#).unwrap();
        assert_eq!(f.maps, ["livonia"]);
        let f: Filters = serde_json::from_str(r#"{"map":""}"#).unwrap();
        assert!(f.maps.is_empty());
        let f: Filters = serde_json::from_str(r#"{"maps":["livonia","enoch","Sakhal"]}"#).unwrap();
        assert_eq!(f.maps, ["livonia", "sakhal"]);
    }

    #[test]
    fn names() {
        assert_eq!(clean_name(" A\u{1}B\n "), "AB");
        assert_eq!(map_title("enoch"), "Livonia");
        assert_eq!(map_title("takistanplus"), "Takistanplus");
        assert_eq!(map_key("enoch"), map_key("Livonia"));
        assert_eq!(map_key("DeerIsle"), "deer isle");
    }
}
