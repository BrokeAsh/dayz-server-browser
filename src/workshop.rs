//! Downloading Workshop mods through the helper program (`dayz-browser-workshop`), which talks to
//! the running Steam client the way the official launcher does. It's a separate, short-lived
//! process: while it's connected, Steam treats DayZ as running (and won't start the game), so it
//! exits as soon as the downloads finish. It reports progress as JSON lines on stdout and stops
//! if its stdin closes.

use anyhow::{Context, Result};
use serde::Deserialize;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

const EXE: &str = if cfg!(windows) {
    "dayz-browser-workshop.exe"
} else {
    "dayz-browser-workshop"
};

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "event", rename_all = "lowercase")]
pub enum Event {
    /// Connected to Steam.
    Ready,
    Item {
        id: u64,
        state: ItemState,
        #[serde(default)]
        done: u64,
        #[serde(default)]
        total: u64,
        #[serde(default)]
        message: Option<String>,
    },
    Error {
        message: String,
    },
    /// Every item is installed or failed.
    Done,
    /// Not sent by the helper: its process ended.
    #[serde(skip)]
    Exited,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ItemState {
    Subscribing,
    Queued,
    Downloading,
    Installed,
    Failed,
}

/// Helpers started whose output hasn't ended yet (so they may still be downloading).
static RUNNING: AtomicUsize = AtomicUsize::new(0);

/// Whether a helper is running, from any thread (the updater asks, as it would replace it).
pub fn running() -> bool {
    RUNNING.load(Ordering::Relaxed) > 0
}

/// The helper next to this program, if it's there.
pub fn helper() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let path = exe.parent()?.join(EXE);
    path.is_file().then_some(path)
}

/// A running helper. Dropping it closes the helper's stdin, which tells it to stop.
pub struct Helper {
    /// `None` once handed to the thread that waits for it to stop.
    child: Option<Child>,
    stdin: Option<ChildStdin>,
}

impl Helper {
    /// Starts downloading (subscribing first where needed) these Workshop items.
    pub fn start(ids: &[u64], report: impl Fn(Event) + Send + 'static) -> Result<Self> {
        let path = helper().context("the Workshop helper program is missing")?;
        let mut command = Command::new(&path);
        command
            .args(ids.iter().map(u64::to_string))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(crate::win::CREATE_NO_WINDOW);
        }
        let mut child = command
            .spawn()
            .with_context(|| format!("couldn't start {}", path.display()))?;
        let stdout = child.stdout.take().context("no helper output")?;
        let stderr = child.stderr.take().context("no helper output")?;
        let stdin = child.stdin.take();
        RUNNING.fetch_add(1, Ordering::Relaxed);
        // Anything on stderr means it failed in a way it couldn't report (such as Valve's library
        // missing), so pass the last of it on.
        let errors = std::thread::spawn(move || {
            let mut text = String::new();
            let _ = std::io::Read::read_to_string(&mut BufReader::new(stderr), &mut text);
            text
        });
        std::thread::spawn(move || {
            let mut reached_steam = false;
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                match serde_json::from_str::<Event>(&line) {
                    Ok(event) => {
                        reached_steam |= matches!(event, Event::Ready | Event::Error { .. });
                        report(event);
                    }
                    Err(e) => log::debug!("helper said {line:?} ({e})"),
                }
            }
            // Valve's library always prints a few lines there when it connects; they only matter
            // if the helper never got as far as reporting.
            let text = errors.join().unwrap_or_default();
            let text = text.trim();
            log::debug!("helper stderr: {text}");
            if !reached_steam && !text.is_empty() {
                let tail: String = text
                    .chars()
                    .rev()
                    .take(300)
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .collect();
                report(Event::Error {
                    message: format!("The Workshop helper failed: {tail}"),
                });
            }
            RUNNING.fetch_sub(1, Ordering::Relaxed);
            report(Event::Exited);
        });
        Ok(Self {
            child: Some(child),
            stdin,
        })
    }

    /// Asks the helper to stop (closing its stdin), and makes sure it does, without waiting here:
    /// a thread gives it two seconds, then kills it.
    pub fn cancel(&mut self) {
        self.stdin = None;
        let Some(mut child) = self.child.take() else {
            return;
        };
        std::thread::spawn(move || {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
            while std::time::Instant::now() < deadline {
                if matches!(child.try_wait(), Ok(Some(_))) {
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            let _ = child.kill();
            let _ = child.wait();
        });
    }
}

impl Drop for Helper {
    fn drop(&mut self) {
        self.cancel();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_helper_events() {
        let e: Event = serde_json::from_str(
            r#"{"event":"item","id":1559212036,"state":"downloading","done":10,"total":100}"#,
        )
        .unwrap();
        assert!(matches!(
            e,
            Event::Item {
                id: 1559212036,
                state: ItemState::Downloading,
                done: 10,
                total: 100,
                ..
            }
        ));
        let e: Event = serde_json::from_str(r#"{"event":"ready"}"#).unwrap();
        assert!(matches!(e, Event::Ready));
    }
}
