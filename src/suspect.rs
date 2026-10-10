//! Spotting listings that are likely fake or the same server again, for the "Hide likely fakes
//! and duplicates" filter (on by default). The rules, measured on DZSA's list of about 10,000:
//!
//! - **Listed twice:** another listing has the same IP and game port, which only one server can
//!   use. (Listings with the same query address too are dropped when the list is read.)
//! - **Copies:** listings with the same name, map, and mods, at least two of them with players
//!   on, are one server listed again or clones made to take up room in lists. Default names
//!   from hosts ("nitrado.net gameserver", "DayZ Server") don't count, since those are many
//!   different servers, and neither do the game's own servers.
//! - **Impossible numbers:** more players than slots, more slots than DayZ allows, or a queue
//!   longer than a server can have.
//! - **Disagrees:** the server's own answers keep saying far fewer players than the list (60
//!   listed, 2 there).
//!
//! Of listings that look like the same server, the best one stays: not one that was asked and
//! didn't answer, then the busiest, then the first in the list. Nothing here hides the user's own
//! servers (favorites, recent ones, servers added by address); the window takes care of that.

use std::cmp::Reverse;
use std::collections::HashMap;
use std::time::{Duration, Instant};

use crate::server::{Server, map_key};

/// Why a listing looks fake or like a second one. `of` is the index of the listing shown instead.
#[derive(Debug, Clone, PartialEq)]
pub enum Suspect {
    ListedTwice { of: usize },
    Copy { of: usize },
    TooManyPlayers { players: u32, slots: u32 },
    TooManySlots(u32),
    LongQueue(u32),
    Disagrees { listed: u32, answered: u32 },
}

/// DayZ servers have at most this many slots.
const MAX_SLOTS: u32 = 127;
/// A queue longer than this is made up.
const MAX_QUEUE: u32 = 255;

/// Names hosts give servers until their owners rename them, normalized (see [`normalize`]).
const DEFAULT_NAMES: &[&str] = &[
    "nitrado.net gameserver",
    "dayz server",
    "dayz standalone server",
    "dayzsa server",
    "hosted by gtxgaming.co.uk",
    "dayz standalone by g-portal.com",
    "dayz server by g-portal.com",
    "dayz server by hosthavoc.com",
    "4netplayers dayz server",
    "a cybrancee hosted dayz server",
    "hosted by survivalhost.org",
    "a shockbyte server",
    "amp powered dayz server",
    "dayz server - powered by minestrator",
    "example name",
    "test",
];

/// Lower case, with runs of spaces as one.
fn normalize(name: &str) -> String {
    name.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// What the rules read about a listing besides the list: the server's latest answer.
#[derive(Debug, Clone, Copy, Default)]
pub struct Answer {
    /// It was asked and didn't answer.
    pub silent: bool,
    /// Players and slots.
    pub players: Option<(u32, u32)>,
    pub queue: Option<u32>,
    /// Its answers have kept disagreeing with the list (see [`Disagreements`]).
    pub disagrees: bool,
}

/// Listings that may be the same server, found once per list.
#[derive(Debug, Default)]
pub struct Groups {
    /// By IP and game port.
    twins: Vec<Vec<usize>>,
    /// By name, map, and mods.
    copies: Vec<Vec<usize>>,
}

type CopyKey = (String, String, bool, Vec<(u64, String)>);

impl Groups {
    pub fn new(servers: &[Server]) -> Self {
        let mut twins: HashMap<(&str, u16), Vec<usize>> = HashMap::new();
        let mut copies: HashMap<CopyKey, Vec<usize>> = HashMap::new();
        for (i, s) in servers.iter().enumerate() {
            twins.entry((&s.ip, s.game_port)).or_default().push(i);
            let name = normalize(&s.name);
            if s.official || DEFAULT_NAMES.contains(&name.as_str()) {
                continue;
            }
            let mut mods: Vec<(u64, String)> = s
                .mods
                .iter()
                .map(|m| (m.id, m.name.to_lowercase()))
                .collect();
            mods.sort();
            copies
                .entry((name, map_key(&s.map), s.modded, mods))
                .or_default()
                .push(i);
        }
        // In list order, so the result doesn't depend on the hash order.
        fn several<K>(groups: HashMap<K, Vec<usize>>) -> Vec<Vec<usize>> {
            let mut groups: Vec<Vec<usize>> =
                groups.into_values().filter(|g| g.len() > 1).collect();
            groups.sort();
            groups
        }
        Self {
            twins: several(twins),
            copies: several(copies),
        }
    }
}

/// Flags each listing that looks fake or like a second one. `answers` goes with `servers`.
pub fn flag(servers: &[Server], answers: &[Answer], groups: &Groups) -> Vec<Option<Suspect>> {
    let answer = |i: usize| answers.get(i).copied().unwrap_or_default();
    let numbers = |i: usize| {
        let s = &servers[i];
        let a = answer(i);
        let (players, slots) = a.players.unwrap_or((s.players, s.max_players));
        (players, slots, a.queue.or(s.queue).unwrap_or(0))
    };
    let mut flags: Vec<Option<Suspect>> = (0..servers.len())
        .map(|i| {
            let (players, slots, queue) = numbers(i);
            if slots > 0 && players > slots {
                Some(Suspect::TooManyPlayers { players, slots })
            } else if slots > MAX_SLOTS {
                Some(Suspect::TooManySlots(slots))
            } else if queue > MAX_QUEUE {
                Some(Suspect::LongQueue(queue))
            } else if answer(i).disagrees {
                Some(Suspect::Disagrees {
                    listed: servers[i].players,
                    answered: players,
                })
            } else {
                None
            }
        })
        .collect();
    // The listing to keep of several: not one that was asked and didn't answer, then the
    // busiest, then the first. One not asked yet isn't counted against (the one on screen is
    // asked first), and pings aren't compared (they jitter), so the pick doesn't flip around.
    let best = |group: &[usize]| {
        group
            .iter()
            .copied()
            .min_by_key(|&i| (answer(i).silent, Reverse(numbers(i).0), i))
    };
    for (group, twins) in groups
        .twins
        .iter()
        .map(|g| (g, true))
        .chain(groups.copies.iter().map(|g| (g, false)))
    {
        // Listings already flagged are out of the running.
        let left: Vec<usize> = group
            .iter()
            .copied()
            .filter(|&i| i < flags.len() && flags[i].is_none())
            .collect();
        if left.len() < 2 || (!twins && left.iter().filter(|&&i| numbers(i).0 > 0).count() < 2) {
            continue;
        }
        let Some(keep) = best(&left) else {
            continue;
        };
        for i in left.into_iter().filter(|&i| i != keep) {
            flags[i] = Some(if twins {
                Suspect::ListedTwice { of: keep }
            } else {
                Suspect::Copy { of: keep }
            });
        }
    }
    flags
}

impl Suspect {
    /// The listing shown instead of this one, for a second listing.
    pub fn other(&self) -> Option<usize> {
        match self {
            Self::ListedTwice { of } | Self::Copy { of } => Some(*of),
            _ => None,
        }
    }

    /// Why, in a sentence, for the details panel.
    pub fn explain(&self, servers: &[Server]) -> String {
        let address = |i: &usize| {
            servers
                .get(*i)
                .map_or_else(String::new, |s| format!("{}:{}", s.ip, s.game_port))
        };
        match self {
            Self::ListedTwice { of } => {
                let name = servers.get(*of).map_or("", |s| s.name.as_str());
                format!("Listed twice: \u{201c}{name}\u{201d} has the same address.")
            }
            Self::Copy { of } => format!(
                "Looks like a copy of the server at {}, which has the same name, map, and mods.",
                address(of)
            ),
            Self::TooManyPlayers { players, slots } => {
                format!("It says it has {players} players on {slots} slots.")
            }
            Self::TooManySlots(slots) => {
                format!("It says it has {slots} slots; DayZ servers have at most {MAX_SLOTS}.")
            }
            Self::LongQueue(queue) => {
                format!("It says {queue} players are waiting, more than a server can queue.")
            }
            Self::Disagrees { listed, answered } => {
                format!("Its own answer says {answered} players; the list says {listed}.")
            }
        }
    }
}

/// Whether an answer says far fewer players than the list: at least 20 fewer, and under a
/// quarter of the list's count.
pub fn disagree(listed: u32, answered: u32) -> bool {
    listed >= answered.saturating_add(20) && answered.saturating_mul(4) < listed
}

/// One answer with far fewer players than the list isn't enough: the server may have just
/// restarted, dropping everyone. It counts once its answers have kept disagreeing for this long.
const CONFIRM_AFTER: Duration = Duration::from_secs(120);

/// Servers whose answers disagree with the list, by `ip:query_port`.
#[derive(Debug, Default)]
pub struct Disagreements(HashMap<String, Disagreement>);

#[derive(Debug)]
struct Disagreement {
    /// The list's count the answers disagreed with.
    listed: u32,
    since: Instant,
    confirmed: bool,
}

impl Disagreements {
    /// Notes an answer from a listed server: `listed` players in the list, `answered` in its
    /// answer.
    pub fn answer(&mut self, addr: &str, listed: u32, answered: u32, now: Instant) {
        if !disagree(listed, answered) {
            self.0.remove(addr);
            return;
        }
        let entry = self.0.entry(addr.to_string()).or_insert(Disagreement {
            listed,
            since: now,
            confirmed: false,
        });
        if entry.listed != listed {
            // A new list with another count: start over.
            *entry = Disagreement {
                listed,
                since: now,
                confirmed: false,
            };
        }
        if now.saturating_duration_since(entry.since) >= CONFIRM_AFTER {
            entry.confirmed = true;
        }
    }

    /// Whether a server's answers have kept disagreeing with the list's count, `listed`. Once the
    /// list says something else (it was refreshed), they no longer count until they disagree
    /// again, so a hidden server comes back to be asked again.
    pub fn confirmed(&self, addr: &str, listed: u32) -> bool {
        self.0
            .get(addr)
            .is_some_and(|d| d.confirmed && d.listed == listed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::ModRef;

    fn server(name: &str, ip: &str, game_port: u16, players: u32) -> Server {
        Server {
            name: name.into(),
            map: "chernarusplus".into(),
            ip: ip.into(),
            game_port,
            query_port: game_port + 1,
            players,
            max_players: 60,
            mods: vec![ModRef {
                id: 1559212036,
                name: "CF".into(),
            }],
            modded: true,
            ..Default::default()
        }
    }

    fn flags(servers: &[Server], answers: &[Answer]) -> Vec<Option<Suspect>> {
        flag(servers, answers, &Groups::new(servers))
    }

    #[test]
    fn listed_twice() {
        let servers = [
            server("dayz.example.de", "203.0.113.1", 2302, 0),
            server("Another name", "203.0.113.1", 2302, 3),
            server("Same IP, other port", "203.0.113.1", 2402, 0),
        ];
        let f = flags(&servers, &[]);
        // The busier one stays.
        assert_eq!(f[0], Some(Suspect::ListedTwice { of: 1 }));
        assert_eq!(f[1], None);
        assert_eq!(f[2], None);
        // A busier one that was asked and didn't answer loses to one that did.
        let answers = [
            Answer {
                players: Some((0, 60)),
                ..Default::default()
            },
            Answer {
                silent: true,
                ..Default::default()
            },
        ];
        let f = flags(&servers, &answers);
        assert_eq!(f[0], None);
        assert_eq!(f[1], Some(Suspect::ListedTwice { of: 0 }));
        assert!(
            f[1].as_ref()
                .unwrap()
                .explain(&servers)
                .contains("dayz.example.de")
        );
        // One not asked yet isn't worse than one that answered: which of two is on screen (and
        // so asked) doesn't decide which stays.
        let mut quiet = servers.clone();
        quiet[1].players = 0;
        let answers = [
            Answer::default(),
            Answer {
                players: Some((0, 60)),
                ..Default::default()
            },
        ];
        assert_eq!(
            flags(&quiet, &answers)[1],
            Some(Suspect::ListedTwice { of: 0 })
        );
    }

    #[test]
    fn copies_need_players_on_several() {
        let mut servers = vec![
            server("Best  Server | PvP", "203.0.113.1", 2302, 10),
            server("best server | pvp", "203.0.113.2", 2302, 40),
            server("Best Server | PvP", "203.0.113.3", 2302, 0),
        ];
        let f = flags(&servers, &[]);
        assert_eq!(f[0], Some(Suspect::Copy { of: 1 }));
        assert_eq!(f[1], None);
        assert_eq!(f[2], Some(Suspect::Copy { of: 1 }));
        assert!(
            f[0].as_ref()
                .unwrap()
                .explain(&servers)
                .contains("203.0.113.2:2302")
        );
        // Only one with players: maybe a server and its old listings, nothing to hide.
        servers[0].players = 0;
        assert_eq!(flags(&servers, &[]), [None, None, None]);
        // Other mods or another map: different servers.
        servers[0].players = 10;
        servers[0].mods.clear();
        servers[0].modded = false;
        servers[2].map = "enoch".into();
        assert_eq!(flags(&servers, &[]), [None, None, None]);
    }

    #[test]
    fn default_names_and_official_servers_are_not_copies() {
        let mut servers = vec![
            server("Nitrado.net  Gameserver", "203.0.113.1", 2302, 3),
            server("nitrado.net gameserver", "203.0.113.2", 2302, 5),
            server("1234 | EUROPE - DE", "203.0.113.3", 2302, 30),
            server("1234 | EUROPE - DE", "203.0.113.4", 2302, 20),
        ];
        servers[2].official = true;
        servers[3].official = true;
        assert_eq!(flags(&servers, &[]), [None, None, None, None]);
    }

    #[test]
    fn impossible_numbers() {
        let mut servers = vec![
            server("A", "203.0.113.1", 2302, 61),
            server("B", "203.0.113.2", 2302, 255),
            server("C", "203.0.113.3", 2302, 10),
            server("D", "203.0.113.4", 2302, 10),
        ];
        servers[1].max_players = 255;
        servers[2].queue = Some(300);
        let f = flags(&servers, &[]);
        assert_eq!(
            f[0],
            Some(Suspect::TooManyPlayers {
                players: 61,
                slots: 60
            })
        );
        assert_eq!(f[1], Some(Suspect::TooManySlots(255)));
        assert_eq!(f[2], Some(Suspect::LongQueue(300)));
        assert_eq!(f[3], None);
        // The answer wins over the list.
        let answers = [Answer {
            players: Some((12, 60)),
            ..Default::default()
        }];
        assert_eq!(flags(&servers, &answers)[0], None);
        // A flagged listing isn't the one kept of two.
        let servers = [
            server("Twin", "203.0.113.9", 2302, 70),
            server("Twin", "203.0.113.9", 2302, 5),
        ];
        let f = flags(&servers, &[]);
        assert!(matches!(f[0], Some(Suspect::TooManyPlayers { .. })));
        assert_eq!(f[1], None);
    }

    #[test]
    fn disagreements_need_confirming() {
        assert!(disagree(60, 2));
        assert!(!disagree(60, 15), "a quarter isn't far enough off");
        assert!(!disagree(15, 0), "small servers empty out for real");
        assert!(!disagree(10, 40));
        let start = Instant::now();
        let mut d = Disagreements::default();
        d.answer("a", 60, 2, start);
        assert!(!d.confirmed("a", 60), "one answer could be a restart");
        d.answer("a", 60, 3, start + Duration::from_secs(60));
        assert!(!d.confirmed("a", 60));
        d.answer("a", 60, 3, start + Duration::from_secs(150));
        assert!(d.confirmed("a", 60));
        // An answer that agrees clears it.
        d.answer("a", 60, 50, start + Duration::from_secs(200));
        assert!(!d.confirmed("a", 60));
        d.answer("b", 60, 1, start);
        d.answer("b", 60, 1, start + CONFIRM_AFTER);
        assert!(d.confirmed("b", 60));
        // A refreshed list with another count doesn't keep it hidden, and starts over.
        assert!(!d.confirmed("b", 58));
        d.answer("b", 58, 1, start + CONFIRM_AFTER * 2);
        assert!(!d.confirmed("b", 58));

        let servers = [server("A", "203.0.113.1", 2302, 60)];
        let answers = [Answer {
            players: Some((2, 60)),
            disagrees: true,
            ..Default::default()
        }];
        let f = flags(&servers, &answers);
        assert_eq!(
            f[0],
            Some(Suspect::Disagrees {
                listed: 60,
                answered: 2
            })
        );
        assert_eq!(
            f[0].as_ref().unwrap().explain(&servers),
            "Its own answer says 2 players; the list says 60."
        );
    }
}
