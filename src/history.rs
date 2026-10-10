//! Player counts over time for the servers the user cares about, recorded while the browser is
//! open. Sites that keep such history want a paid key (BattleMetrics) or have no API, so the
//! browser keeps its own.
//!
//! Each server has a file, `history/<ip>_<query port>.bin` in the data folder: a 4-byte header,
//! then fixed-size records, oldest first. A new sample is appended; records older than two weeks
//! are dropped when the file is rewritten, about once a day. A file that's unreadable or ends in
//! half a record (a crash while writing) is read as far as it makes sense and rewritten with the
//! next sample.
//!
//! From the record, [`restarts`] finds when a server restarted (it stopped answering for a few
//! minutes, or its in-game clock was set back), and [`restart_schedule`] finds the times of day
//! it usually restarts at, when they follow a clear pattern.

use std::collections::{BTreeSet, HashMap};
use std::io::Write;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::a2s::{self, Info};
use crate::settings::write_atomic;

/// How long samples are kept, in minutes.
pub const KEEP: u32 = 14 * DAY;
/// How often the servers are asked.
pub const SAMPLE_EVERY: Duration = Duration::from_secs(5 * 60);
/// The first round, a little after start so it doesn't compete with getting the list.
const FIRST_SAMPLE: Duration = Duration::from_secs(20);
/// An answer to an ordinary ping is recorded only if the last sample is at least this old
/// (minutes), so a server on screen doesn't fill its file.
const PING_GAP: u32 = 3;
/// How long one query waits for an answer; a server that doesn't answer is asked once more.
const QUERY_TIMEOUT: Duration = Duration::from_secs(2);
/// Servers asked at the same time in a round.
const PARALLEL: usize = 16;

const DAY: u32 = 24 * 60;
const MAGIC: &[u8; 4] = b"DZH1";
const RECORD: usize = 12;
const ANSWERED: u8 = 1;
const NO_TIME: u16 = u16::MAX;

/// One look at a server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sample {
    /// Unix time in minutes.
    pub minute: u32,
    /// Players online and slots; both 0 when the server didn't answer.
    pub players: u16,
    pub slots: u16,
    /// The in-game time, in minutes after midnight, when the server said.
    pub game_time: Option<u16>,
    pub answered: bool,
}

impl Sample {
    /// A server's answer at `minute`.
    pub fn from_info(info: &Info, minute: u32) -> Self {
        let clamp = |n: u32| n.min(u32::from(u16::MAX - 1)) as u16;
        Self {
            minute,
            players: clamp(info.players),
            slots: clamp(info.max_players),
            game_time: a2s::parse_tags(&info.keywords)
                .time
                .as_deref()
                .and_then(parse_clock),
            answered: true,
        }
    }

    /// A server that didn't answer at `minute`.
    pub fn silent(minute: u32) -> Self {
        Self {
            minute,
            players: 0,
            slots: 0,
            game_time: None,
            answered: false,
        }
    }
}

/// `HH:MM` as minutes after midnight.
fn parse_clock(time: &str) -> Option<u16> {
    let (h, m) = time.split_once(':')?;
    let (h, m): (u16, u16) = (h.parse().ok()?, m.parse().ok()?);
    (h < 24 && m < 60).then_some(h * 60 + m)
}

/// The current Unix time in minutes.
pub fn now_minute() -> u32 {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    u32::try_from(secs / 60).unwrap_or(u32::MAX)
}

/// A record: minute (`u32`), players, slots, in-game time (`u16`, `FFFF` when unknown), flags,
/// and a spare byte, all little-endian.
pub fn encode(s: &Sample) -> [u8; RECORD] {
    let mut out = [0u8; RECORD];
    out[0..4].copy_from_slice(&s.minute.to_le_bytes());
    out[4..6].copy_from_slice(&s.players.to_le_bytes());
    out[6..8].copy_from_slice(&s.slots.to_le_bytes());
    out[8..10].copy_from_slice(&s.game_time.unwrap_or(NO_TIME).to_le_bytes());
    out[10] = if s.answered { ANSWERED } else { 0 };
    out
}

/// Reads a record; `None` if it can't be one (unknown flags, a time of day past midnight).
pub fn decode(bytes: &[u8]) -> Option<Sample> {
    let bytes: &[u8; RECORD] = bytes.try_into().ok()?;
    let u16_at = |i: usize| u16::from_le_bytes([bytes[i], bytes[i + 1]]);
    let minute = u32::from_le_bytes(bytes[0..4].try_into().ok()?);
    let game_time = match u16_at(8) {
        NO_TIME => None,
        t if t < 24 * 60 => Some(t),
        _ => return None,
    };
    if minute == 0 || bytes[10] & !ANSWERED != 0 || bytes[11] != 0 {
        return None;
    }
    Some(Sample {
        minute,
        players: u16_at(4),
        slots: u16_at(6),
        game_time,
        answered: bytes[10] & ANSWERED != 0,
    })
}

/// Reads a whole file: its samples, oldest first, and whether the file is in order (its header
/// is right, it holds whole records only, all of them readable and in time order). A file with a
/// wrong header has no samples.
pub fn decode_file(bytes: &[u8]) -> (Vec<Sample>, bool) {
    let Some(records) = bytes.strip_prefix(MAGIC) else {
        return (Vec::new(), false);
    };
    let mut clean = records.len() % RECORD == 0;
    let mut samples = Vec::with_capacity(records.len() / RECORD);
    for record in records.as_chunks::<RECORD>().0 {
        match decode(record) {
            Some(s) => samples.push(s),
            None => clean = false,
        }
    }
    if !samples.is_sorted_by_key(|s| s.minute) {
        clean = false;
        samples.sort_by_key(|s| s.minute);
    }
    (samples, clean)
}

fn encode_file(samples: &[Sample]) -> Vec<u8> {
    let mut out = Vec::with_capacity(MAGIC.len() + samples.len() * RECORD);
    out.extend_from_slice(MAGIC);
    for s in samples {
        out.extend_from_slice(&encode(s));
    }
    out
}

/// A server's file in `dir`, or `None` if `addr` isn't `ip:port`.
pub fn file_for(dir: &Path, addr: &str) -> Option<PathBuf> {
    let (ip, port) = addr.rsplit_once(':')?;
    let port: u16 = port.parse().ok()?;
    // Only what's safe in a file name on any system (IPv6 addresses have colons).
    let ip: String = ip
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' {
                c
            } else {
                '_'
            }
        })
        .collect();
    (!ip.is_empty()).then(|| dir.join(format!("{ip}_{port}.bin")))
}

/// A server's samples from the last two weeks before `now` (Unix minutes), oldest first; none
/// if it has no file or the file is unreadable.
pub fn load(dir: &Path, addr: &str, now: u32) -> Vec<Sample> {
    let Some(path) = file_for(dir, addr) else {
        return Vec::new();
    };
    let Ok(bytes) = std::fs::read(&path) else {
        return Vec::new();
    };
    let (mut samples, _) = decode_file(&bytes);
    let cutoff = now.saturating_sub(KEEP);
    samples.retain(|s| s.minute >= cutoff);
    samples
}

/// Adds a sample to a server's file: appended, or (once a day, or when the file isn't in order)
/// the whole file rewritten without what's older than two weeks.
pub fn append(dir: &Path, addr: &str, sample: &Sample) -> anyhow::Result<()> {
    let path = file_for(dir, addr).ok_or_else(|| anyhow::anyhow!("bad address {addr}"))?;
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(e) => return Err(e.into()),
    };
    let (mut samples, clean) = decode_file(&bytes);
    let cutoff = sample.minute.saturating_sub(KEEP);
    let stale = samples
        .first()
        .is_some_and(|s| s.minute < cutoff.saturating_sub(DAY));
    let behind = samples.last().is_some_and(|s| s.minute > sample.minute);
    if bytes.is_empty() || !clean || stale || behind {
        samples.retain(|s| s.minute >= cutoff);
        samples.push(*sample);
        samples.sort_by_key(|s| s.minute);
        return write_atomic(&path, &encode_file(&samples));
    }
    let mut file = std::fs::OpenOptions::new().append(true).open(&path)?;
    file.write_all(&encode(sample))?;
    Ok(())
}

/// Deletes the files of servers that haven't been recorded for two weeks.
fn clean_up(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let old = Duration::from_secs(u64::from(KEEP + DAY) * 60);
    for entry in entries.flatten() {
        let path = entry.path();
        let is_history = path.extension().is_some_and(|e| e == "bin");
        let modified = entry.metadata().and_then(|m| m.modified()).ok();
        if is_history
            && modified
                .and_then(|m| m.elapsed().ok())
                .is_some_and(|age| age > old)
        {
            let _ = std::fs::remove_file(&path);
        }
    }
}

/// What the recorder reports.
pub enum Event {
    /// A server's samples from its file, as asked for with [`Recorder::load`].
    Loaded(String, Vec<Sample>),
    /// A new sample was added to a server's file.
    Recorded(String, Sample),
}

enum Job {
    Targets(Vec<String>),
    Record(String, Sample),
    Load(String),
    /// The answers (or not) of a round of queries.
    Round(Vec<(String, Sample)>),
}

/// Records the history in the background: asks the target servers every [`SAMPLE_EVERY`] and
/// writes their answers, along with answers the browser got anyway, and reads files for the
/// details panel. All file work happens on its thread.
pub struct Recorder {
    tx: mpsc::Sender<Job>,
}

impl Recorder {
    /// Starts recording into `dir`, reporting through `report`.
    pub fn spawn(dir: PathBuf, report: impl Fn(Event) + Send + 'static) -> Self {
        let (tx, rx) = mpsc::channel();
        let rounds = tx.clone();
        std::thread::Builder::new()
            .name("history".into())
            .spawn(move || run(&dir, &rx, &rounds, &report))
            .expect("can start a thread");
        Self { tx }
    }

    /// Sets the servers (`ip:query_port`) to ask every few minutes.
    pub fn set_targets(&self, targets: Vec<String>) {
        let _ = self.tx.send(Job::Targets(targets));
    }

    /// Records an answer the browser got anyway (a ping), if the last sample isn't recent.
    pub fn record_answer(&self, addr: String, info: &Info) {
        let _ = self
            .tx
            .send(Job::Record(addr, Sample::from_info(info, now_minute())));
    }

    /// Reads a server's samples; they come back as [`Event::Loaded`].
    pub fn load(&self, addr: String) {
        let _ = self.tx.send(Job::Load(addr));
    }
}

fn run(dir: &Path, rx: &mpsc::Receiver<Job>, rounds: &mpsc::Sender<Job>, report: &dyn Fn(Event)) {
    clean_up(dir);
    let mut targets: Vec<String> = Vec::new();
    let mut next_round = Instant::now() + FIRST_SAMPLE;
    let mut asking = false;
    // The minute of each server's latest sample written this run.
    let mut latest: HashMap<String, u32> = HashMap::new();
    let mut silence = Silence::default();
    let mut warned = false;
    let mut write = |addr: String, sample: Sample, latest: &mut HashMap<String, u32>| {
        match append(dir, &addr, &sample) {
            Ok(()) => {
                latest.insert(addr.clone(), sample.minute);
                report(Event::Recorded(addr, sample));
            }
            // Once: a full disk would say so on every sample.
            Err(e) if !warned => {
                warned = true;
                log::warn!("couldn't record the history of {addr}: {e:#}");
            }
            Err(e) => log::debug!("couldn't record the history of {addr}: {e:#}"),
        }
    };
    loop {
        let wait = next_round.saturating_duration_since(Instant::now());
        match rx.recv_timeout(wait) {
            Ok(Job::Targets(new)) => targets = new,
            Ok(Job::Record(addr, sample)) => {
                let recent = latest
                    .get(&addr)
                    .is_some_and(|&m| sample.minute < m.saturating_add(PING_GAP));
                silence.answered(&addr);
                if !recent && targets.contains(&addr) {
                    write(addr, sample, &mut latest);
                }
            }
            Ok(Job::Load(addr)) => {
                report(Event::Loaded(addr.clone(), load(dir, &addr, now_minute())))
            }
            Ok(Job::Round(answers)) => {
                asking = false;
                for (addr, sample) in silence.round(answers) {
                    if targets.contains(&addr) {
                        write(addr, sample, &mut latest);
                    }
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                next_round = Instant::now() + SAMPLE_EVERY;
                if !asking && !targets.is_empty() {
                    asking = true;
                    let (targets, rounds) = (targets.clone(), rounds.clone());
                    std::thread::spawn(move || {
                        let _ = rounds.send(Job::Round(ask_all(&targets)));
                    });
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
        }
    }
}

/// Decides which of a round's answers to record, so this computer's own connection dropping
/// doesn't look like servers restarting. When no server answered a round of several, it's more
/// likely the connection: nothing is recorded. With only one server asked, there's nothing to
/// compare with, so its silence is held back until the next round confirms it (it's silent
/// again) or not (it answered: the silence is dropped).
#[derive(Debug, Default)]
struct Silence {
    /// The lone server's silent sample waiting for the next round.
    held: Option<(String, Sample)>,
    /// The lone server whose silence the last round confirmed, so its next silence is recorded
    /// straight away.
    confirmed: Option<String>,
}

impl Silence {
    fn round(&mut self, answers: Vec<(String, Sample)>) -> Vec<(String, Sample)> {
        let held = self.held.take();
        let confirmed = self.confirmed.take();
        match &answers[..] {
            [(addr, sample)] if !sample.answered => {
                let silent_before = confirmed.as_ref() == Some(addr)
                    || held.as_ref().is_some_and(|(a, _)| a == addr);
                if !silent_before {
                    self.held = answers.into_iter().next();
                    return Vec::new();
                }
                self.confirmed = Some(addr.clone());
                held.filter(|(a, _)| a == addr)
                    .into_iter()
                    .chain(answers)
                    .collect()
            }
            _ if answers.len() >= 2 && answers.iter().all(|(_, s)| !s.answered) => Vec::new(),
            _ => answers,
        }
    }

    /// A server answered a ping between rounds: a silence held back isn't confirmed.
    fn answered(&mut self, addr: &str) {
        if self.held.as_ref().is_some_and(|(a, _)| a == addr) {
            self.held = None;
        }
        if self.confirmed.as_deref() == Some(addr) {
            self.confirmed = None;
        }
    }
}

/// Asks each server for its details, a few at a time.
fn ask_all(targets: &[String]) -> Vec<(String, Sample)> {
    let mut answers = Vec::with_capacity(targets.len());
    for chunk in targets.chunks(PARALLEL) {
        std::thread::scope(|scope| {
            let asked: Vec<_> = chunk
                .iter()
                .map(|addr| scope.spawn(move || (addr.clone(), ask(addr))))
                .collect();
            answers.extend(asked.into_iter().filter_map(|h| h.join().ok()));
        });
    }
    answers
}

fn ask(addr: &str) -> Sample {
    let Ok(target) = addr.parse::<SocketAddr>().or_else(|_| a2s::resolve(addr)) else {
        return Sample::silent(now_minute());
    };
    for _ in 0..2 {
        if let Ok((info, _)) = a2s::info(target, QUERY_TIMEOUT) {
            return Sample::from_info(&info, now_minute());
        }
    }
    Sample::silent(now_minute())
}

/// Longest stretch (minutes, from the last answer before to the first after) a server can be
/// silent for it to count as a restart rather than an outage.
const RESTART_GAP: u32 = 30;
/// Consecutive samples further apart than this (minutes) say nothing about the in-game clock.
const CLOCK_GAP: u32 = 10;
/// Restarts found closer together than this (minutes) are one restart.
const SAME_RESTART: u32 = 30;

/// When a server seems to have restarted, as Unix minutes, oldest first: where it stopped
/// answering for a few minutes between two answers, or where its in-game clock was set back.
pub fn restarts(samples: &[Sample]) -> Vec<u32> {
    let mut found = Vec::new();
    // Silent stretches between two answers.
    let mut last_answer: Option<&Sample> = None;
    let mut first_silent: Option<&Sample> = None;
    for s in samples {
        if !s.answered {
            first_silent = first_silent.or(Some(s));
            continue;
        }
        if let (Some(before), Some(silent)) = (last_answer, first_silent)
            && s.minute.saturating_sub(before.minute) <= RESTART_GAP
        {
            // It went down somewhere between its last answer and the first silence.
            found.push(before.minute + silent.minute.saturating_sub(before.minute) / 2);
        }
        last_answer = Some(s);
        first_silent = None;
    }

    // The in-game clock set back. How fast it runs differs between servers (and at night), so
    // only a step back that its usual pace can't explain counts.
    let pairs: Vec<(&Sample, &Sample, u32, u32)> = samples
        .windows(2)
        .filter_map(|w| {
            let (a, b) = (&w[0], &w[1]);
            let (ga, gb) = (a.game_time?, b.game_time?);
            let real = b.minute.saturating_sub(a.minute);
            let game = (u32::from(gb) + DAY - u32::from(ga)) % DAY;
            (a.answered && b.answered && (1..=CLOCK_GAP).contains(&real))
                .then_some((a, b, real, game))
        })
        .collect();
    let mut paces: Vec<f32> = pairs
        .iter()
        .filter(|(_, _, _, game)| *game <= DAY / 2)
        .map(|&(_, _, real, game)| game as f32 / real as f32)
        .collect();
    if paces.len() >= 12 {
        paces.sort_by(f32::total_cmp);
        let pace = paces[paces.len() * 9 / 10];
        for &(a, b, real, game) in &pairs {
            if game > DAY / 2 && game as f32 > 3.0 * pace * real as f32 + 60.0 {
                found.push(a.minute + (b.minute - a.minute) / 2);
            }
        }
    }

    found.sort_unstable();
    let mut merged: Vec<u32> = Vec::new();
    for t in found {
        if merged.last().is_none_or(|&last| t - last >= SAME_RESTART) {
            merged.push(t);
        }
    }
    merged
}

/// Restarts this close (minutes) to a time of day count as at that time.
const TOLERANCE: i64 = 20;

/// The times of day a server usually restarts at (local minutes after midnight, in order), when
/// its restarts follow a clear pattern: each time seen on at least two days, and on at least half
/// the days the browser was watching then; together most of the restarts seen. `offset` gives the
/// local time zone's offset from UTC (minutes) at a Unix minute.
pub fn restart_schedule(samples: &[Sample], offset: impl Fn(u32) -> i32) -> Option<Vec<u16>> {
    let events: Vec<i64> = restarts(samples)
        .into_iter()
        .map(|t| i64::from(t) + i64::from(offset(t)))
        .collect();
    if events.len() < 3 {
        return None;
    }
    let watched: Vec<i64> = samples
        .iter()
        .map(|s| i64::from(s.minute) + i64::from(offset(s.minute)))
        .collect();
    let day = i64::from(DAY);
    // Distance between two times of day, around midnight.
    let apart = |a: i64, b: i64| {
        let d = (a - b).rem_euclid(day);
        d.min(day - d)
    };
    // Which day a moment counts as for a time of day: the one whose `time` is nearest to it.
    let day_of = |moment: i64, time: i64| (moment - time + day / 2).div_euclid(day);

    let mut left: Vec<i64> = events.clone();
    let mut times: Vec<u16> = Vec::new();
    let mut explained = 0;
    while !left.is_empty() {
        // The time of day with the most restarts near it, then all of those.
        let busiest = *left
            .iter()
            .max_by_key(|&&t| left.iter().filter(|&&u| apart(t, u) <= TOLERANCE).count())?;
        let (near, rest): (Vec<i64>, Vec<i64>) =
            left.iter().partition(|&&u| apart(busiest, u) <= TOLERANCE);
        left = rest;
        let center = circular_mean(&near, day);
        let days: BTreeSet<i64> = near.iter().map(|&t| day_of(t, center)).collect();
        // Days the browser was watching then: with samples shortly before and after.
        let (mut before, mut after) = (BTreeSet::new(), BTreeSet::new());
        for &t in &watched {
            let since = (t - center + day / 2).rem_euclid(day) - day / 2;
            if (-TOLERANCE..0).contains(&since) {
                before.insert(day_of(t, center));
            } else if (0..=TOLERANCE).contains(&since) {
                after.insert(day_of(t, center));
            }
        }
        let watching = before.intersection(&after).count();
        if days.len() >= 2 && days.len() * 2 >= watching {
            explained += near.len();
            // To the quarter hour: restarts are seen a few minutes either way.
            let quarter = ((center + 7) / 15 * 15).rem_euclid(day);
            times.push(quarter as u16);
        }
    }
    times.sort_unstable();
    times.dedup();
    let clear = !times.is_empty() && times.len() <= 8 && explained * 4 >= events.len() * 3;
    clear.then_some(times)
}

/// The average time of day of `times` (local minutes), around midnight.
fn circular_mean(times: &[i64], day: i64) -> i64 {
    let (mut x, mut y) = (0.0f64, 0.0f64);
    for &t in times {
        let angle = (t.rem_euclid(day) as f64) / day as f64 * std::f64::consts::TAU;
        x += angle.cos();
        y += angle.sin();
    }
    let angle = y.atan2(x).rem_euclid(std::f64::consts::TAU);
    (angle / std::f64::consts::TAU * day as f64).round() as i64 % day
}

#[cfg(test)]
mod tests {
    use super::*;

    fn up(minute: u32, players: u16) -> Sample {
        Sample {
            minute,
            players,
            slots: 60,
            game_time: None,
            answered: true,
        }
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "dayz-browser-history-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn silence_needs_confirming() {
        let one = |s: Sample| vec![("a".to_string(), s)];
        let minutes = |kept: Vec<(String, Sample)>| -> Vec<u32> {
            kept.into_iter().map(|(_, s)| s.minute).collect()
        };
        let mut silence = Silence::default();
        assert_eq!(minutes(silence.round(one(up(0, 5)))), [0]);
        // One favorite and a network blip: the silence is dropped, so no restart is made up.
        assert!(silence.round(one(Sample::silent(5))).is_empty());
        assert_eq!(minutes(silence.round(one(up(10, 5)))), [10]);
        // Silent twice in a row: both are recorded, and so is any more silence after.
        assert!(silence.round(one(Sample::silent(15))).is_empty());
        assert_eq!(minutes(silence.round(one(Sample::silent(20)))), [15, 20]);
        assert_eq!(minutes(silence.round(one(Sample::silent(25)))), [25]);
        assert_eq!(minutes(silence.round(one(up(30, 0)))), [30]);
        // An answer to a ping in between: not confirmed.
        assert!(silence.round(one(Sample::silent(35))).is_empty());
        silence.answered("a");
        assert!(silence.round(one(Sample::silent(40))).is_empty());
        // Several servers: all silent is this computer, otherwise it's theirs.
        let both = |a: Sample, b: Sample| vec![("a".to_string(), a), ("b".to_string(), b)];
        assert!(
            silence
                .round(both(Sample::silent(45), Sample::silent(45)))
                .is_empty()
        );
        assert_eq!(
            minutes(silence.round(both(Sample::silent(50), up(50, 3)))),
            [50, 50]
        );
    }

    #[test]
    fn records_round_trip() {
        let samples = [
            Sample {
                minute: 29_000_000,
                players: 42,
                slots: 60,
                game_time: Some(17 * 60 + 30),
                answered: true,
            },
            Sample::silent(29_000_005),
            Sample {
                minute: 29_000_010,
                players: 0,
                slots: 127,
                game_time: Some(0),
                answered: true,
            },
        ];
        for s in &samples {
            assert_eq!(decode(&encode(s)), Some(*s));
        }
        let (read, clean) = decode_file(&encode_file(&samples));
        assert!(clean);
        assert_eq!(read, samples);

        let info = Info {
            players: 70_000,
            max_players: 60,
            keywords: "battleye,no3rd,lqs0,mod,06:05".into(),
            ..Default::default()
        };
        let s = Sample::from_info(&info, 5);
        assert_eq!(
            (s.players, s.slots, s.game_time),
            (u16::MAX - 1, 60, Some(365))
        );
        assert_eq!(parse_clock("24:00"), None);
        assert_eq!(parse_clock("23:59"), Some(1439));
    }

    #[test]
    fn reads_damaged_files() {
        let samples = [up(1000, 1), up(1005, 2), up(1010, 3)];
        let good = encode_file(&samples);
        // Cut off in the middle of the last record.
        let (read, clean) = decode_file(&good[..good.len() - 5]);
        assert_eq!(read, samples[..2]);
        assert!(!clean);
        // Not a history file at all.
        assert_eq!(decode_file(b"hello, world"), (Vec::new(), false));
        assert_eq!(decode_file(b""), (Vec::new(), false));
        // A garbled record is skipped, the rest kept.
        let mut garbled = good.clone();
        garbled[4 + RECORD + 10] = 0xff;
        let (read, clean) = decode_file(&garbled);
        assert_eq!(read, [samples[0], samples[2]]);
        assert!(!clean);
        // Out of order (the clock was set back): sorted.
        let (read, clean) = decode_file(&encode_file(&[up(1010, 3), up(1000, 1)]));
        assert_eq!(read, [up(1000, 1), up(1010, 3)]);
        assert!(!clean);
    }

    #[test]
    fn appends_prunes_and_repairs() {
        let dir = scratch("append");
        let addr = "203.0.113.5:27016";
        let path = file_for(&dir, addr).unwrap();
        assert!(path.ends_with("203.0.113.5_27016.bin"));
        let now = 30_000_000;

        // A new file, then appends.
        append(&dir, addr, &up(now - 10, 1)).unwrap();
        append(&dir, addr, &up(now - 5, 2)).unwrap();
        append(&dir, addr, &Sample::silent(now)).unwrap();
        let read = load(&dir, addr, now);
        assert_eq!(read, [up(now - 10, 1), up(now - 5, 2), Sample::silent(now)]);
        let size = std::fs::metadata(&path).unwrap().len();
        assert_eq!(size, (MAGIC.len() + 3 * RECORD) as u64);

        // Over two weeks old: not shown, and dropped once the file is a day past that.
        std::fs::write(
            &path,
            encode_file(&[up(now - KEEP - DAY - 1, 9), up(now - KEEP + 1, 8)]),
        )
        .unwrap();
        assert!(load(&dir, addr, now + 2).is_empty());
        assert_eq!(load(&dir, addr, now), [up(now - KEEP + 1, 8)]);
        append(&dir, addr, &up(now, 7)).unwrap();
        let (read, clean) = decode_file(&std::fs::read(&path).unwrap());
        assert!(clean);
        assert_eq!(read, [up(now - KEEP + 1, 8), up(now, 7)]);

        // Half a record at the end (a crash while writing): rewritten whole.
        let mut bytes = std::fs::read(&path).unwrap();
        bytes.extend_from_slice(&[1, 2, 3]);
        std::fs::write(&path, &bytes).unwrap();
        append(&dir, addr, &up(now + 5, 6)).unwrap();
        let (read, clean) = decode_file(&std::fs::read(&path).unwrap());
        assert!(clean);
        // (The rewrite also drops what has turned two weeks old since.)
        assert_eq!(read, [up(now, 7), up(now + 5, 6)]);

        // Garbage: started over.
        std::fs::write(&path, b"not a history file").unwrap();
        assert!(load(&dir, addr, now).is_empty());
        append(&dir, addr, &up(now, 5)).unwrap();
        assert_eq!(load(&dir, addr, now), [up(now, 5)]);

        // No file, or an address that can't be one.
        assert!(load(&dir, "198.51.100.1:2303", now).is_empty());
        assert!(append(&dir, "nonsense", &up(now, 1)).is_err());
        assert!(file_for(&dir, "[2001:db8::1]:27016").is_some_and(|p| {
            p.file_name()
                .is_some_and(|n| n.to_string_lossy() == "_2001_db8__1__27016.bin")
        }));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Samples every 5 minutes for `days` days from midnight UTC, with the server down for a few
    /// minutes at each of `restarts` (minutes after midnight) and its clock set to 08:00 then.
    fn watched(days: u32, restarts: &[u32], skip: impl Fn(u32, u32) -> bool) -> Vec<Sample> {
        let start = 29_000_000 / DAY * DAY;
        let mut samples = Vec::new();
        let mut clock = 8 * 60;
        for minute in (start..start + days * DAY).step_by(5) {
            let (day, time) = ((minute - start) / DAY, minute % DAY);
            if skip(day, time) {
                continue;
            }
            let restarting = restarts
                .iter()
                .find(|&&r| time >= r && time < r + 5 && !skip(day, r));
            if restarting.is_some() {
                samples.push(Sample::silent(minute));
                clock = 8 * 60;
                continue;
            }
            clock = (clock + 5 * 4) % DAY;
            samples.push(Sample {
                game_time: Some(clock as u16),
                ..up(minute, 30)
            });
        }
        samples
    }

    #[test]
    fn finds_restarts() {
        let samples = watched(1, &[6 * 60, 18 * 60], |_, _| false);
        let found = restarts(&samples);
        let day = samples[0].minute;
        assert_eq!(found.len(), 2, "{found:?}");
        assert!(found[0].abs_diff(day + 6 * 60) <= 5);
        assert!(found[1].abs_diff(day + 18 * 60) <= 5);

        // A long outage isn't a restart, and neither is the browser being closed.
        let mut samples = vec![up(1000, 5)];
        samples.extend((1005..1100).step_by(5).map(Sample::silent));
        samples.push(up(1100, 5));
        samples.push(up(2000, 5));
        assert!(restarts(&samples).is_empty());

        // Only the clock tells: no silent sample, but the time went from 18:00 back to 08:00.
        let mut samples: Vec<Sample> = (0..40u32)
            .map(|i| Sample {
                game_time: Some((16 * 60 + i * 5) as u16),
                ..up(1000 + i * 5, 10)
            })
            .collect();
        let jump = samples.last().unwrap().minute + 5;
        samples.push(Sample {
            game_time: Some(8 * 60),
            ..up(jump, 2)
        });
        assert_eq!(restarts(&samples), [jump - 3]);

        // A fast clock wrapping past midnight isn't a restart.
        let samples: Vec<Sample> = (0..60)
            .map(|i| Sample {
                game_time: Some(((i * 5 * 30) % DAY) as u16),
                ..up(1000 + i * 5, 10)
            })
            .collect();
        assert!(restarts(&samples).is_empty());
    }

    #[test]
    fn finds_restart_schedule() {
        let utc = |_| 0;
        // Every six hours, watched around the clock for three days.
        let every_six = [0, 6 * 60, 12 * 60, 18 * 60];
        let samples = watched(3, &every_six, |_, _| false);
        assert_eq!(
            restart_schedule(&samples, utc),
            Some(vec![0, 360, 720, 1080])
        );
        // In local time: two hours ahead of UTC.
        assert_eq!(
            restart_schedule(&samples, |_| 120),
            Some(vec![120, 480, 840, 1200])
        );

        // Watched only in the evenings: only the evening restart is known.
        let evenings = watched(4, &every_six, |_, time| time < 17 * 60);
        assert_eq!(restart_schedule(&evenings, utc), Some(vec![1080]));

        // One day isn't enough to call it a pattern.
        let one_day = watched(1, &every_six, |_, _| false);
        assert_eq!(restart_schedule(&one_day, utc), None);

        // Restarts at a different time each day: no pattern.
        let start = samples[0].minute;
        let mut random: Vec<Sample> = (0..4 * DAY).step_by(5).map(|m| up(start + m, 20)).collect();
        for (day, at) in [(0, 100), (1, 500), (2, 900), (3, 1300)] {
            let i = random
                .iter()
                .position(|s| s.minute == start + day * DAY + at)
                .unwrap();
            random[i] = Sample::silent(random[i].minute);
        }
        assert_eq!(restarts(&random).len(), 4);
        assert_eq!(restart_schedule(&random, utc), None);

        // Every evening, but in the morning on only two of the six days it was watched then:
        // the evening only.
        let samples = watched(6, &[6 * 60, 18 * 60], |day, time| {
            day >= 2 && time == 6 * 60
        });
        assert_eq!(restarts(&samples).len(), 8);
        assert_eq!(restart_schedule(&samples, utc), Some(vec![1080]));

        // Every evening, but as many restarts at other times: not clear enough.
        let mut samples = watched(3, &[18 * 60], |_, _| false);
        for (day, at) in [(0, 200), (1, 600), (2, 1000)] {
            let minute = start + day * DAY + at;
            let i = samples.iter().position(|s| s.minute == minute).unwrap();
            samples[i] = Sample::silent(minute);
        }
        assert_eq!(restarts(&samples).len(), 6);
        assert_eq!(restart_schedule(&samples, utc), None);

        // Around midnight: restarts at 23:55 and 00:05 are the same time of day.
        let mut samples: Vec<Sample> = (0..3 * DAY + 60)
            .step_by(5)
            .map(|m| up(start + m, 20))
            .collect();
        for minute in [start + DAY - 5, start + 2 * DAY + 5, start + 3 * DAY - 5] {
            let i = samples.iter().position(|s| s.minute == minute).unwrap();
            samples[i] = Sample::silent(minute);
        }
        assert_eq!(restart_schedule(&samples, utc), Some(vec![0]));
    }
}
