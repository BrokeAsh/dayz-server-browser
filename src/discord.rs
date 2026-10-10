//! Discord Rich Presence: while DayZ runs after joining from the browser, the user's Discord
//! status shows the server's name and map.
//!
//! Talking to Discord blocks, so it happens on a thread of its own that takes what to show over
//! a channel. Discord needn't be running: the thread keeps trying while there's something to
//! show, and shows it again when Discord starts later or restarts. The crate finds Discord's
//! socket (under `$XDG_RUNTIME_DIR`, including Flatpak and Snap installs) or, on Windows, its
//! named pipe.

use discord_rich_presence::{DiscordIpc, DiscordIpcClient, activity};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

use crate::server::map_title;

/// The browser's Discord application, made at https://discord.com/developers/applications
/// (named "DayZ", since Discord shows "Playing <the application's name>", with an art asset
/// named [`LARGE_IMAGE`]). Its id isn't a secret: every program that shows a presence carries
/// its id. `None` turns the feature off and hides its setting.
pub const DISCORD_APP_ID: Option<&str> = None;

/// The picture shown with the activity: an art asset of that name uploaded to the application.
/// Discord shows none if it's missing.
pub const LARGE_IMAGE: &str = "dayz";
const LARGE_TEXT: &str = "DayZ";

/// Discord's limits for each line of text, which it counts in UTF-16 units; it refuses shorter
/// or longer ones.
const MAX_TEXT: usize = 128;
const MIN_TEXT: usize = 2;

/// The application id to use, if one is set and looks like one (Discord's ids are numbers).
pub fn app_id() -> Option<&'static str> {
    usable(DISCORD_APP_ID)
}

fn usable(id: Option<&str>) -> Option<&str> {
    id.map(str::trim)
        .filter(|id| !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit()))
}

/// What Discord shows: the server's name, then its map and perspective, with the time played.
#[derive(Debug, Clone, PartialEq)]
pub struct Activity {
    pub details: String,
    pub state: Option<String>,
    /// Unix time in milliseconds the game was launched.
    pub start: i64,
}

/// The activity for playing on a server.
pub fn activity(name: &str, map: &str, first_person: bool, start: i64) -> Activity {
    let map = map_title(map);
    let view = if first_person { "1PP" } else { "3PP" };
    let state = if map == "?" {
        view.to_string()
    } else {
        format!("{map} · {view}")
    };
    Activity {
        details: fit(name).unwrap_or_else(|| "On a DayZ server".into()),
        state: fit(&state),
        start,
    }
}

/// Text cut to fit Discord's limit (with "…" when cut), or `None` if it's too short to show.
pub fn fit(text: &str) -> Option<String> {
    let text: String = text.chars().filter(|c| !c.is_control()).collect();
    let text = text.trim();
    if text.encode_utf16().count() < MIN_TEXT {
        return None;
    }
    if text.encode_utf16().count() <= MAX_TEXT {
        return Some(text.to_string());
    }
    let mut cut = String::new();
    let mut units = 0;
    for c in text.chars() {
        // Room for the "…" (one unit).
        if units + c.len_utf16() > MAX_TEXT - 1 {
            break;
        }
        units += c.len_utf16();
        cut.push(c);
    }
    Some(format!("{}…", cut.trim_end()))
}

/// Shows and clears the activity, through the thread that talks to Discord.
pub struct Presence {
    tx: Sender<Command>,
}

enum Command {
    Show(Activity),
    Clear,
    /// Clear, disconnect, and say so.
    Quit(Sender<()>),
}

impl Presence {
    /// Starts the thread. It connects to Discord only once there's something to show.
    pub fn start(app_id: &str) -> Self {
        let (tx, rx) = mpsc::channel();
        let id = app_id.to_string();
        let spawned = std::thread::Builder::new()
            .name("discord".into())
            .spawn(move || run(&rx, || Discord::new(&id), Timing::REAL));
        if let Err(e) = spawned {
            log::warn!("couldn't start the Discord thread: {e}");
        }
        Self { tx }
    }

    pub fn show(&self, activity: Activity) {
        let _ = self.tx.send(Command::Show(activity));
    }

    pub fn clear(&self) {
        let _ = self.tx.send(Command::Clear);
    }

    /// Clears the activity and disconnects, waiting a moment at most (Discord may be hung).
    pub fn stop(&self) {
        let (done, wait) = mpsc::channel();
        if self.tx.send(Command::Quit(done)).is_ok() {
            let _ = wait.recv_timeout(Duration::from_secs(1));
        }
    }
}

/// A connection to Discord; the real one, or a stand-in in tests.
trait Ipc {
    fn connect(&mut self) -> Result<(), String>;
    fn show(&mut self, activity: &Activity) -> Result<(), String>;
    fn clear(&mut self) -> Result<(), String>;
    fn close(&mut self);
}

struct Discord {
    client: DiscordIpcClient,
}

impl Discord {
    fn new(app_id: &str) -> Self {
        Self {
            client: DiscordIpcClient::new(app_id),
        }
    }

    /// Reads Discord's answer to a command. Reading it also notices a Discord that went away,
    /// and keeps unread answers from piling up.
    fn answer(&mut self) -> Result<(), String> {
        let (op, reply) = self.client.recv().map_err(|e| e.to_string())?;
        let message = || {
            reply
                .pointer("/data/message")
                .or_else(|| reply.get("message"))
                .and_then(|m| m.as_str())
                .unwrap_or("no reason given")
                .to_string()
        };
        // 2 closes the connection (as for an unknown application id).
        if op == 2 {
            return Err(format!("Discord closed the connection: {}", message()));
        }
        if reply.get("evt").and_then(|e| e.as_str()) == Some("ERROR") {
            return Err(format!("Discord refused the activity: {}", message()));
        }
        Ok(())
    }
}

impl Ipc for Discord {
    fn connect(&mut self) -> Result<(), String> {
        self.client.connect().map_err(|e| e.to_string())
    }

    fn show(&mut self, a: &Activity) -> Result<(), String> {
        let mut payload = activity::Activity::new()
            .details(a.details.as_str())
            .timestamps(activity::Timestamps::new().start(a.start))
            .assets(
                activity::Assets::new()
                    .large_image(LARGE_IMAGE)
                    .large_text(LARGE_TEXT),
            );
        if let Some(state) = &a.state {
            payload = payload.state(state.as_str());
        }
        self.client
            .set_activity(payload)
            .map_err(|e| e.to_string())?;
        self.answer()
    }

    fn clear(&mut self) -> Result<(), String> {
        self.client.clear_activity().map_err(|e| e.to_string())?;
        self.answer()
    }

    fn close(&mut self) {
        let _ = self.client.close();
    }
}

#[derive(Clone, Copy)]
struct Timing {
    /// After a failed attempt, try again after this long, then twice as long each time...
    retry: Duration,
    /// ...up to this.
    max_retry: Duration,
    /// While connected, send the activity again this often, to notice a Discord that quit or
    /// restarted (and show it in the new one).
    refresh: Duration,
}

impl Timing {
    const REAL: Self = Self {
        retry: Duration::from_secs(15),
        max_retry: Duration::from_secs(120),
        refresh: Duration::from_secs(60),
    };
}

/// The thread: keeps Discord showing the latest activity asked for (or nothing), until told to
/// quit or the browser drops its end of the channel.
fn run<I: Ipc>(rx: &Receiver<Command>, mut connect: impl FnMut() -> I, timing: Timing) {
    let mut conn: Option<I> = None;
    let mut wanted: Option<Activity> = None;
    // Discord shows `wanted`, or nothing when that's `None`.
    let mut in_sync = true;
    let mut next_try = Instant::now();
    let mut retry = timing.retry;
    let disconnect = |conn: &mut Option<I>| {
        if let Some(mut c) = conn.take() {
            if let Err(e) = c.clear() {
                log::debug!("Discord: couldn't clear the activity: {e}");
            }
            c.close();
        }
    };
    loop {
        let command = if wanted.is_some() {
            rx.recv_timeout(next_try.saturating_duration_since(Instant::now()))
        } else {
            rx.recv().map_err(|_| RecvTimeoutError::Disconnected)
        };
        match command {
            Ok(Command::Show(activity)) => {
                if wanted.as_ref() != Some(&activity) {
                    wanted = Some(activity);
                    in_sync = false;
                    next_try = Instant::now();
                }
            }
            Ok(Command::Clear) => {
                wanted = None;
                in_sync = false;
            }
            Ok(Command::Quit(done)) => {
                disconnect(&mut conn);
                let _ = done.send(());
                return;
            }
            Err(RecvTimeoutError::Disconnected) => {
                disconnect(&mut conn);
                return;
            }
            Err(RecvTimeoutError::Timeout) => {}
        }
        let Some(activity) = &wanted else {
            // Nothing to show: no need to stay connected.
            if !in_sync {
                disconnect(&mut conn);
                in_sync = true;
                retry = timing.retry;
            }
            continue;
        };
        if Instant::now() < next_try {
            continue;
        }
        let shown = match conn.as_mut() {
            Some(c) => c.show(activity),
            None => {
                let mut c = connect();
                let shown = c.connect().and_then(|()| c.show(activity));
                conn = Some(c);
                shown
            }
        };
        match shown {
            Ok(()) => {
                if !in_sync {
                    log::info!("Discord shows the server being played");
                }
                in_sync = true;
                retry = timing.retry;
                next_try = Instant::now() + timing.refresh;
            }
            Err(e) => {
                // Usually Discord isn't running; try again later, less often each time.
                log::debug!("Discord: {e}");
                if let Some(mut c) = conn.take() {
                    c.close();
                }
                in_sync = false;
                next_try = Instant::now() + retry;
                retry = (retry * 2).min(timing.max_retry);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    /// A made-up application id, as the real one would be.
    const DUMMY_ID: &str = "123456789012345678";

    #[test]
    fn application_id() {
        assert_eq!(usable(Some(DUMMY_ID)), Some(DUMMY_ID));
        assert_eq!(usable(Some(" 42 ")), Some("42"));
        assert_eq!(usable(Some("")), None);
        assert_eq!(usable(Some("paste the id here")), None);
        assert_eq!(usable(None), None);
    }

    #[test]
    fn activity_text() {
        let a = activity(
            "  My Server | PvE  ",
            "chernarusplus",
            true,
            1_700_000_000_000,
        );
        assert_eq!(a.details, "My Server | PvE");
        assert_eq!(a.state.as_deref(), Some("Chernarus · 1PP"));
        assert_eq!(a.start, 1_700_000_000_000);
        let a = activity("Deer\tIsle", "deerisle", false, 0);
        assert_eq!(a.details, "DeerIsle");
        assert_eq!(a.state.as_deref(), Some("Deer Isle · 3PP"));
        // Discord refuses one-letter lines.
        let a = activity("x", "", false, 0);
        assert_eq!(a.details, "On a DayZ server");
        assert_eq!(a.state.as_deref(), Some("3PP"));
    }

    #[test]
    fn fits_discords_limits() {
        assert_eq!(fit(""), None);
        assert_eq!(fit(" a "), None);
        assert_eq!(fit("ab").as_deref(), Some("ab"));
        let exact = "x".repeat(MAX_TEXT);
        assert_eq!(fit(&exact).as_deref(), Some(exact.as_str()));
        let long = fit(&"y".repeat(300)).unwrap();
        assert_eq!(long.encode_utf16().count(), MAX_TEXT);
        assert!(long.ends_with("y…"));
        // Characters outside the BMP count twice, and aren't split.
        let emoji = fit(&"🧟".repeat(100)).unwrap();
        assert!(emoji.encode_utf16().count() <= MAX_TEXT);
        assert_eq!(emoji, format!("{}…", "🧟".repeat(63)));
        // A cut at a space doesn't leave it before the "…".
        let spaced = fit(&format!("{} tail", "z".repeat(126))).unwrap();
        assert_eq!(spaced, format!("{}…", "z".repeat(126)));
    }

    #[derive(Debug, Clone, PartialEq)]
    enum Call {
        Connect,
        Show(String),
        Clear,
        Close,
    }

    /// A pretend Discord: logs calls, and is "running" while `up` is set.
    struct Fake {
        log: Arc<Mutex<Vec<Call>>>,
        up: Arc<Mutex<bool>>,
    }

    impl Ipc for Fake {
        fn connect(&mut self) -> Result<(), String> {
            self.log.lock().unwrap().push(Call::Connect);
            if *self.up.lock().unwrap() {
                Ok(())
            } else {
                Err("failed to find IPC socket".into())
            }
        }
        fn show(&mut self, a: &Activity) -> Result<(), String> {
            self.log.lock().unwrap().push(Call::Show(a.details.clone()));
            if *self.up.lock().unwrap() {
                Ok(())
            } else {
                Err("broken pipe".into())
            }
        }
        fn clear(&mut self) -> Result<(), String> {
            self.log.lock().unwrap().push(Call::Clear);
            Ok(())
        }
        fn close(&mut self) {
            self.log.lock().unwrap().push(Call::Close);
        }
    }

    fn wait_for(log: &Arc<Mutex<Vec<Call>>>, what: impl Fn(&[Call]) -> bool) {
        let start = Instant::now();
        while !what(&log.lock().unwrap()) {
            assert!(
                start.elapsed() < Duration::from_secs(5),
                "{:?}",
                log.lock().unwrap()
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn keeps_discord_showing_it() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let up = Arc::new(Mutex::new(false));
        let (tx, rx) = mpsc::channel();
        let timing = Timing {
            retry: Duration::from_millis(20),
            max_retry: Duration::from_millis(40),
            refresh: Duration::from_millis(50),
        };
        let thread = {
            let (log, up) = (log.clone(), up.clone());
            std::thread::spawn(move || {
                run(
                    &rx,
                    || Fake {
                        log: log.clone(),
                        up: up.clone(),
                    },
                    timing,
                )
            })
        };
        // Nothing to show: it doesn't connect.
        std::thread::sleep(Duration::from_millis(60));
        assert!(log.lock().unwrap().is_empty());

        // Discord isn't running: it keeps trying...
        tx.send(Command::Show(activity("Server One", "enoch", false, 1)))
            .unwrap();
        wait_for(&log, |l| {
            l.iter().filter(|c| **c == Call::Connect).count() >= 3
        });
        assert!(
            !log.lock()
                .unwrap()
                .iter()
                .any(|c| matches!(c, Call::Show(_)))
        );

        // ...and shows it once Discord starts.
        *up.lock().unwrap() = true;
        wait_for(&log, |l| l.contains(&Call::Show("Server One".into())));

        // Discord restarts: the refresh notices, and it shows it again.
        *up.lock().unwrap() = false;
        log.lock().unwrap().clear();
        wait_for(&log, |l| l.contains(&Call::Connect));
        *up.lock().unwrap() = true;
        log.lock().unwrap().clear();
        wait_for(&log, |l| l.contains(&Call::Show("Server One".into())));

        // Cleared: it clears and disconnects, and stays quiet.
        tx.send(Command::Clear).unwrap();
        wait_for(&log, |l| l.ends_with(&[Call::Clear, Call::Close]));
        log.lock().unwrap().clear();
        std::thread::sleep(Duration::from_millis(120));
        assert!(log.lock().unwrap().is_empty());

        // Quitting while showing clears it first.
        tx.send(Command::Show(activity("Server Two", "", true, 2)))
            .unwrap();
        wait_for(&log, |l| l.contains(&Call::Show("Server Two".into())));
        let (done, wait) = mpsc::channel();
        tx.send(Command::Quit(done)).unwrap();
        wait.recv_timeout(Duration::from_secs(5)).unwrap();
        thread.join().unwrap();
        assert!(log.lock().unwrap().ends_with(&[Call::Clear, Call::Close]));
    }
}
