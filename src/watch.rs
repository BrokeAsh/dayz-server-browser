//! The notification watch list's background side. One thread asks each watched server for its
//! details about every 30 seconds, spread out, and decides when to notify: when a server goes
//! from no room (full, queued, or not answering) to room, the way the join window's "Join when
//! there's room" sees it. A server that keeps getting and losing a free slot isn't notified again
//! until it has been without room for a couple of minutes.
//!
//! Notifications go out from this thread, not the window's, so they arrive even while the window
//! is minimized and not drawing.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, Instant};

use crate::a2s::{self, Info, Pong};
use crate::notify;
use crate::server::map_title;

/// How often each watched server is asked.
pub const EVERY: Duration = Duration::from_secs(30);
/// After a check without an answer, ask again this soon: one lost reply isn't "not answering".
const RETRY: Duration = Duration::from_secs(5);
/// Checks in a row without an answer before a server counts as not answering.
const SILENT_AFTER: u32 = 2;
/// After a notification, a server has to be without room this long to be notified again.
pub const REARM: Duration = Duration::from_secs(2 * 60);
const TIMEOUT: Duration = Duration::from_secs(2);
/// A server's name and map are cut to this many characters in a notification.
const TEXT_MAX: usize = 100;
/// At start, the first checks wait this long, out of the way of loading the list.
const START_DELAY: Duration = Duration::from_secs(3);

/// What a watched server's checks say.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// Not asked yet, or it missed one answer so far.
    Checking,
    /// A free slot and nobody queued.
    Room { players: u32, slots: u32 },
    /// Full, or players are queued.
    NoRoom {
        players: u32,
        slots: u32,
        queue: u32,
    },
    /// It didn't answer, several times in a row.
    Silent,
}

impl Status {
    pub fn room(self) -> bool {
        matches!(self, Status::Room { .. })
    }
}

/// A server's answer, as far as room goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Answer {
    pub players: u32,
    pub slots: u32,
    /// Players queued; 0 when the server's tags don't say.
    pub queue: u32,
}

impl Answer {
    pub fn from_info(info: &Info) -> Self {
        Self {
            players: info.players,
            slots: info.max_players,
            queue: a2s::parse_tags(&info.keywords).queue.unwrap_or(0),
        }
    }

    fn status(self) -> Status {
        let Answer {
            players,
            slots,
            queue,
        } = self;
        if slots > 0 && players < slots && queue == 0 {
            Status::Room { players, slots }
        } else {
            Status::NoRoom {
                players,
                slots,
                queue,
            }
        }
    }
}

/// What to do after a check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    /// It has room after having none: notify.
    Notify,
    /// The first clear answer for a server the user just added, to tell them how it stands.
    Added,
}

/// One watched server's checks so far, and whether the next room is worth a notification.
#[derive(Debug, Clone)]
pub struct Tracker {
    /// Added by the user just now (not at start): its first clear answer is reported to them
    /// rather than notified.
    added: bool,
    status: Status,
    /// Checks in a row without an answer.
    misses: u32,
    /// Since when it has been without room, while it is.
    without_since: Option<Instant>,
    /// Whether room after no room notifies. Not after a notification, until the server has been
    /// without room for `REARM`.
    armed: bool,
}

impl Tracker {
    /// `added`: the user just added it, rather than it being on the list from before.
    pub fn new(added: bool) -> Self {
        Self {
            added,
            status: Status::Checking,
            misses: 0,
            without_since: None,
            armed: true,
        }
    }

    pub fn status(&self) -> Status {
        self.status
    }

    /// The last check got no answer, but too few in a row to say it's not answering.
    fn unsure(&self) -> bool {
        self.misses > 0 && self.misses < SILENT_AFTER
    }

    /// Takes in a check made at `now`: the server's answer, or `None` if it didn't answer.
    pub fn observe(&mut self, now: Instant, answer: Option<Answer>) -> Option<Event> {
        self.status = match answer {
            Some(answer) => {
                self.misses = 0;
                answer.status()
            }
            None => {
                self.misses += 1;
                if self.misses < SILENT_AFTER {
                    // Maybe just a lost reply: it stands as it did.
                    return None;
                }
                Status::Silent
            }
        };
        let added = std::mem::take(&mut self.added);
        if self.status.room() {
            let without = self.without_since.take();
            if without.is_some_and(|since| now.saturating_duration_since(since) >= REARM) {
                self.armed = true;
            }
            if added {
                Some(Event::Added)
            } else if without.is_some() && self.armed {
                self.armed = false;
                Some(Event::Notify)
            } else {
                // Room in its first answer at start: nothing changed that's worth a notification.
                None
            }
        } else {
            let since = *self.without_since.get_or_insert(now);
            if now.saturating_duration_since(since) >= REARM {
                self.armed = true;
            }
            added.then_some(Event::Added)
        }
    }
}

/// After DayZ is started on a server, a room notification for it is noise for this long: the
/// game is on its way in.
const JOINING_QUIET: Duration = Duration::from_secs(3 * 60);

/// Servers being joined, which get no room notifications: the ones the join window waits on
/// (it starts DayZ by itself when there's room), and the last one DayZ was started on.
#[derive(Debug, Default)]
struct Joining {
    waiting: Vec<String>,
    launched: Option<(String, Instant)>,
}

impl Joining {
    fn quiet(&self, addr: &str, now: Instant) -> bool {
        self.waiting.iter().any(|a| a == addr)
            || self.launched.as_ref().is_some_and(|(a, at)| {
                a == addr && now.saturating_duration_since(*at) < JOINING_QUIET
            })
    }
}

static JOINING: std::sync::Mutex<Joining> = std::sync::Mutex::new(Joining {
    waiting: Vec::new(),
    launched: None,
});

fn joining<T>(f: impl FnOnce(&mut Joining) -> T) -> T {
    f(&mut JOINING.lock().unwrap_or_else(|e| e.into_inner()))
}

/// The join window started waiting for room on a server (`ip:query_port`).
pub fn waiting_to_join(addr: &str) {
    joining(|j| j.waiting.push(addr.to_string()));
}

/// The join window stopped waiting for room on a server.
pub fn done_waiting(addr: &str) {
    joining(|j| {
        if let Some(i) = j.waiting.iter().position(|a| a == addr) {
            j.waiting.remove(i);
        }
    });
}

/// DayZ was started on a server.
pub fn launched(addr: &str) {
    joining(|j| j.launched = Some((addr.to_string(), Instant::now())));
}

/// What the watcher reports to the window.
pub enum Out {
    /// A watched server was asked. `added`: this was the first clear answer for a server the
    /// user just added.
    Checked {
        pong: Pong,
        status: Status,
        added: bool,
    },
    /// A notification for this server (`ip:query_port`), or its Join button, was clicked.
    Clicked(String),
    /// A notification couldn't be shown (no notification service); this says what it said.
    NotShown(String),
}

enum Cmd {
    /// The servers watched from before, at start.
    Resume(Vec<String>),
    /// One the user just added.
    Watch(String),
    Unwatch(String),
}

type Report = Arc<dyn Fn(Out) + Send + Sync>;

/// The watch list's checker. It stops when dropped.
pub struct Watcher {
    tx: mpsc::Sender<Cmd>,
    sound: Arc<AtomicBool>,
}

impl Watcher {
    pub fn spawn(sound: bool, report: impl Fn(Out) + Send + Sync + 'static) -> Self {
        let (tx, rx) = mpsc::channel();
        let sound = Arc::new(AtomicBool::new(sound));
        let shared = sound.clone();
        let report: Report = Arc::new(report);
        std::thread::Builder::new()
            .name("watch list".into())
            .spawn(move || run(&rx, &shared, &report))
            .expect("can start a thread");
        Self { tx, sound }
    }

    /// Starts checking the servers watched from before, spread over the first round.
    pub fn resume(&self, addrs: Vec<String>) {
        let _ = self.tx.send(Cmd::Resume(addrs));
    }

    /// Starts checking a server the user just added, straight away.
    pub fn watch(&self, addr: &str) {
        let _ = self.tx.send(Cmd::Watch(addr.to_string()));
    }

    pub fn unwatch(&self, addr: &str) {
        let _ = self.tx.send(Cmd::Unwatch(addr.to_string()));
    }

    pub fn set_sound(&self, on: bool) {
        self.sound.store(on, Ordering::Relaxed);
    }
}

struct Entry {
    tracker: Tracker,
    due: Instant,
    /// Changes when it's removed and added again, so a check that was under way meanwhile
    /// doesn't count for the new one.
    generation: u64,
}

fn run(rx: &mpsc::Receiver<Cmd>, sound: &AtomicBool, report: &Report) {
    let mut entries: HashMap<String, Entry> = HashMap::new();
    let mut generation = 0;
    let mut apply = |entries: &mut HashMap<String, Entry>, cmd: Cmd| {
        let now = Instant::now();
        let mut add = |addr: String, tracker: Tracker, due: Instant| {
            generation += 1;
            entries.insert(
                addr,
                Entry {
                    tracker,
                    due,
                    generation,
                },
            );
        };
        match cmd {
            Cmd::Resume(addrs) => {
                let gap = EVERY / addrs.len().max(1) as u32;
                for (i, addr) in addrs.into_iter().enumerate() {
                    add(
                        addr,
                        Tracker::new(false),
                        now + START_DELAY + gap * i as u32,
                    );
                }
            }
            Cmd::Watch(addr) => add(addr, Tracker::new(true), now),
            Cmd::Unwatch(addr) => {
                entries.remove(&addr);
            }
        }
    };
    loop {
        // Commands first, waiting for them until the next check is due.
        let next = entries
            .iter()
            .min_by_key(|(_, e)| e.due)
            .map(|(addr, e)| (addr.clone(), e.due, e.generation));
        let cmd = match &next {
            Some((_, due, _)) => rx.recv_timeout(due.saturating_duration_since(Instant::now())),
            None => rx.recv().map_err(|_| RecvTimeoutError::Disconnected),
        };
        match cmd {
            Ok(cmd) => {
                apply(&mut entries, cmd);
                continue;
            }
            // The browser is closing.
            Err(RecvTimeoutError::Disconnected) => return,
            Err(RecvTimeoutError::Timeout) => {}
        }
        let Some((addr, _, asked_generation)) = next else {
            continue;
        };
        let answer = a2s::resolve(&addr)
            .and_then(|target| a2s::info(target, TIMEOUT))
            .ok();
        // Changes to the list while asking, so a server removed meanwhile isn't notified.
        while let Ok(cmd) = rx.try_recv() {
            apply(&mut entries, cmd);
        }
        let pong = Pong {
            addr: addr.clone(),
            ping: answer
                .as_ref()
                .map(|(_, took)| (took.as_millis() as u32).max(1)),
            info: answer.map(|(info, _)| info),
        };
        let Some(entry) = entries
            .get_mut(&addr)
            .filter(|e| e.generation == asked_generation)
        else {
            report(Out::Checked {
                pong,
                status: Status::Checking,
                added: false,
            });
            continue;
        };
        let now = Instant::now();
        let event = entry
            .tracker
            .observe(now, pong.info.as_ref().map(Answer::from_info));
        entry.due = now + if entry.tracker.unsure() { RETRY } else { EVERY };
        if event == Some(Event::Notify)
            && let Some(info) = &pong.info
        {
            if joining(|j| j.quiet(&addr, now)) {
                log::info!("{addr} has room, but it's being joined: no notification");
            } else {
                notify_room(&addr, info, sound.load(Ordering::Relaxed), report);
            }
        }
        report(Out::Checked {
            status: entry.tracker.status(),
            added: event == Some(Event::Added),
            pong,
        });
    }
}

/// Tells the user a server has room, on its own thread (showing it can take a moment, and on
/// Linux the thread then waits a few minutes at most for a click). Falls back to the window if
/// it can't be shown.
fn notify_room(addr: &str, info: &Info, sound: bool, report: &Report) {
    // The server's own words, so kept to plain text of a sensible length.
    let name = match notify::server_text(&info.name, TEXT_MAX) {
        name if name.is_empty() => addr.to_string(),
        name => name,
    };
    let title = format!("Room on {name}");
    let body = format!(
        "{}/{} players, no queue · {}",
        info.players,
        info.max_players,
        notify::server_text(&map_title(&info.map), TEXT_MAX)
    );
    log::info!("notifying: {title} ({body})");
    let (addr, report) = (addr.to_string(), report.clone());
    let spawned = std::thread::Builder::new()
        .name("notification".into())
        .spawn(move || {
            let clicked: notify::OnClick = {
                let report = report.clone();
                Box::new(move || report(Out::Clicked(addr)))
            };
            if let Err(e) = notify::show(&title, &body, sound, Some(clicked)) {
                log::warn!("couldn't show a notification ({e:#}); showing it in the window");
                report(Out::NotShown(format!("{title}: {body}")));
            }
        });
    if let Err(e) = spawned {
        log::warn!("couldn't start a thread for a notification: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FULL: Answer = Answer {
        players: 60,
        slots: 60,
        queue: 0,
    };
    const QUEUED: Answer = Answer {
        players: 59,
        slots: 60,
        queue: 3,
    };
    const ROOM: Answer = Answer {
        players: 57,
        slots: 60,
        queue: 0,
    };

    fn secs(start: Instant, s: u64) -> Instant {
        start + Duration::from_secs(s)
    }

    #[test]
    fn room_means_a_free_slot_and_no_queue() {
        assert!(ROOM.status().room());
        assert!(!FULL.status().room());
        assert!(!QUEUED.status().room());
        // A server saying it has no slots at all isn't one to join.
        let none = Answer {
            players: 0,
            slots: 0,
            queue: 0,
        };
        assert!(!none.status().room());
        // No queue tag counts as nobody queued.
        let info = Info {
            players: 10,
            max_players: 60,
            keywords: "battleye,no3rd,mod,17:30".into(),
            ..Default::default()
        };
        assert!(Answer::from_info(&info).status().room());
        let info = Info {
            keywords: "battleye,lqs2,17:30".into(),
            ..info
        };
        assert_eq!(Answer::from_info(&info).queue, 2);
        assert!(!Answer::from_info(&info).status().room());
    }

    #[test]
    fn notifies_when_a_full_server_gets_room() {
        let t0 = Instant::now();
        let mut t = Tracker::new(false);
        assert_eq!(t.observe(t0, Some(FULL)), None);
        assert_eq!(t.observe(secs(t0, 30), Some(QUEUED)), None);
        assert_eq!(t.observe(secs(t0, 60), Some(ROOM)), Some(Event::Notify));
        assert_eq!(
            t.status(),
            Status::Room {
                players: 57,
                slots: 60
            }
        );
        // Still room: nothing new.
        assert_eq!(t.observe(secs(t0, 90), Some(ROOM)), None);
    }

    #[test]
    fn room_at_start_is_not_news() {
        let t0 = Instant::now();
        let mut t = Tracker::new(false);
        assert_eq!(t.observe(t0, Some(ROOM)), None);
        // But losing it and getting it back is.
        assert_eq!(t.observe(secs(t0, 30), Some(FULL)), None);
        assert_eq!(t.observe(secs(t0, 60), Some(ROOM)), Some(Event::Notify));
    }

    #[test]
    fn a_new_server_is_reported_once_however_it_stands() {
        let t0 = Instant::now();
        let mut t = Tracker::new(true);
        assert_eq!(t.observe(t0, Some(ROOM)), Some(Event::Added));
        assert_eq!(t.observe(secs(t0, 30), Some(ROOM)), None);
        // Room after that is a notification, as for any other.
        assert_eq!(t.observe(secs(t0, 60), Some(FULL)), None);
        assert_eq!(t.observe(secs(t0, 90), Some(ROOM)), Some(Event::Notify));

        let mut t = Tracker::new(true);
        assert_eq!(t.observe(t0, Some(FULL)), Some(Event::Added));
        assert_eq!(t.observe(secs(t0, 30), Some(ROOM)), Some(Event::Notify));

        // One that doesn't answer is reported once it counts as not answering.
        let mut t = Tracker::new(true);
        assert_eq!(t.observe(t0, None), None);
        assert_eq!(t.status(), Status::Checking);
        assert_eq!(t.observe(secs(t0, 5), None), Some(Event::Added));
        assert_eq!(t.status(), Status::Silent);
    }

    #[test]
    fn no_new_notification_until_it_was_without_room_a_while() {
        let t0 = Instant::now();
        let mut t = Tracker::new(false);
        t.observe(t0, Some(FULL));
        assert_eq!(t.observe(secs(t0, 30), Some(ROOM)), Some(Event::Notify));
        // A slot that comes and goes: quiet.
        assert_eq!(t.observe(secs(t0, 60), Some(FULL)), None);
        assert_eq!(t.observe(secs(t0, 90), Some(ROOM)), None);
        assert_eq!(t.observe(secs(t0, 120), Some(QUEUED)), None);
        assert_eq!(t.observe(secs(t0, 150), Some(ROOM)), None);
        // Full for two minutes, then room: worth telling again.
        assert_eq!(t.observe(secs(t0, 180), Some(FULL)), None);
        assert_eq!(t.observe(secs(t0, 240), Some(FULL)), None);
        assert_eq!(t.observe(secs(t0, 300), Some(ROOM)), Some(Event::Notify));
        // Measured from the first check without room, even if the checks are far apart.
        let mut t = Tracker::new(false);
        t.observe(t0, Some(FULL));
        assert_eq!(t.observe(secs(t0, 30), Some(ROOM)), Some(Event::Notify));
        t.observe(secs(t0, 60), Some(FULL));
        assert_eq!(t.observe(secs(t0, 180), Some(ROOM)), Some(Event::Notify));
    }

    #[test]
    fn one_lost_reply_is_not_a_restart() {
        let t0 = Instant::now();
        let mut t = Tracker::new(false);
        t.observe(t0, Some(ROOM));
        assert_eq!(t.observe(secs(t0, 30), None), None);
        assert!(t.unsure());
        // It still stands as it did.
        assert!(t.status().room());
        assert_eq!(t.observe(secs(t0, 35), Some(ROOM)), None);
        assert!(!t.unsure());
    }

    #[test]
    fn servers_being_joined_are_quiet() {
        let t0 = Instant::now();
        let mut j = Joining::default();
        assert!(!j.quiet("a", t0));
        j.waiting.push("a".into());
        assert!(j.quiet("a", t0) && !j.quiet("b", t0));
        j.waiting.clear();
        j.launched = Some(("a".into(), t0));
        assert!(j.quiet("a", secs(t0, 60)));
        assert!(!j.quiet("b", secs(t0, 60)));
        assert!(!j.quiet("a", t0 + JOINING_QUIET), "long since started");
    }

    #[test]
    fn notifies_when_a_silent_server_comes_back_with_room() {
        let t0 = Instant::now();
        let mut t = Tracker::new(false);
        t.observe(t0, Some(ROOM));
        // A restart: no answers.
        assert_eq!(t.observe(secs(t0, 30), None), None);
        assert_eq!(t.observe(secs(t0, 35), None), None);
        assert_eq!(t.status(), Status::Silent);
        assert!(!t.unsure());
        assert_eq!(t.observe(secs(t0, 65), None), None);
        assert_eq!(t.observe(secs(t0, 95), Some(ROOM)), Some(Event::Notify));
    }
}
