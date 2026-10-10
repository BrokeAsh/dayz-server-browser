//! Downloads DayZ Workshop items through the running Steam client, for `dayz-browser`.
//!
//! Usage: `dayz-browser-workshop <item id>...`. It connects to Steam as DayZ (so Steam shows DayZ
//! as running until it exits), subscribes to each item that isn't subscribed yet, asks Steam to
//! download it (or its update), and reports progress as one JSON object per line:
//!
//! - `{"event":"ready"}` once connected,
//! - `{"event":"item","id":…,"state":"subscribing|queued|downloading|installed|failed","done":…,"total":…}`,
//! - `{"event":"error","message":…}` if it can't connect,
//! - `{"event":"done"}` when every item is installed or failed.
//!
//! `dayz-browser-workshop --unsubscribe <item id>...` unsubscribes from each item instead,
//! reporting `"state":"unsubscribing|unsubscribed|failed"` for each, then `done`. Steam deletes an
//! unsubscribed item's files once DayZ (which this helper counts as) is no longer running.
//!
//! It stops early when its stdin closes (the browser cancelled or quit).

use serde_json::json;
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};
use steamworks::{Client, ItemState, PublishedFileId};

const DAYZ: u32 = 221100;
/// Give up on what's left when no download has moved for this long. Measured across all items:
/// Steam downloads one at a time, so the ones queued behind a big mod wait without progress.
const STALL: Duration = Duration::from_secs(600);
/// Ask Steam again if a subscribed item hasn't started downloading after this long.
const NUDGE: Duration = Duration::from_secs(15);
/// Give up on unsubscribing when Steam hasn't answered for this long.
const UNSUBSCRIBE_TIMEOUT: Duration = Duration::from_secs(60);

fn emit(value: serde_json::Value) {
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{value}");
    let _ = out.flush();
}

struct Item {
    id: u64,
    reported: Option<(&'static str, u64, u64)>,
    last_progress: (u64, u64),
    last_nudge: Instant,
    finished: bool,
}

impl Item {
    fn report(&mut self, state: &'static str, done: u64, total: u64, message: Option<String>) {
        if self.reported == Some((state, done, total)) && message.is_none() {
            return;
        }
        self.reported = Some((state, done, total));
        let mut event =
            json!({"event": "item", "id": self.id, "state": state, "done": done, "total": total});
        if let Some(message) = message {
            event["message"] = json!(message);
        }
        emit(event);
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let unsubscribing = args.first().is_some_and(|a| a == "--unsubscribe");
    let ids: Vec<u64> = args.iter().filter_map(|a| a.parse().ok()).collect();
    if ids.is_empty() {
        emit(json!({"event": "error", "message": "no Workshop items given"}));
        std::process::exit(2);
    }

    let stop = Arc::new(AtomicBool::new(false));
    {
        let stop = stop.clone();
        std::thread::spawn(move || {
            let mut buf = [0u8; 64];
            let mut stdin = std::io::stdin();
            while matches!(stdin.read(&mut buf), Ok(n) if n > 0) {}
            stop.store(true, Ordering::SeqCst);
        });
    }

    let client = match Client::init_app(DAYZ) {
        Ok(client) => client,
        Err(e) => {
            emit(json!({
                "event": "error",
                "message": format!(
                    "couldn't connect to Steam; is it running and signed in to the account that \
                     owns DayZ? ({e})"
                ),
            }));
            std::process::exit(1);
        }
    };
    emit(json!({"event": "ready"}));
    if unsubscribing {
        unsubscribe(&client, &ids, &stop);
        return;
    }
    let ugc = client.ugc();

    let (tx, rx) = mpsc::channel::<(u64, Result<(), String>)>();
    let now = Instant::now();
    let mut items: Vec<Item> = ids
        .iter()
        .map(|&id| Item {
            id,
            reported: None,
            last_progress: (0, 0),
            last_nudge: now,
            finished: false,
        })
        .collect();
    for item in &mut items {
        let file = PublishedFileId(item.id);
        if ugc.item_state(file).contains(ItemState::SUBSCRIBED) {
            ugc.download_item(file, true);
        } else {
            item.report("subscribing", 0, 0, None);
            let tx = tx.clone();
            let id = item.id;
            ugc.subscribe_item(file, move |result| {
                let _ = tx.send((id, result.map_err(|e| e.to_string())));
            });
        }
    }

    // When any item last moved (progress, or finishing).
    let mut last_activity = Instant::now();
    loop {
        if stop.load(Ordering::SeqCst) {
            std::process::exit(3);
        }
        client.run_callbacks();
        while let Ok((id, result)) = rx.try_recv() {
            let Some(item) = items.iter_mut().find(|i| i.id == id) else {
                continue;
            };
            match result {
                Ok(()) => {
                    ugc.download_item(PublishedFileId(id), true);
                    item.last_nudge = Instant::now();
                }
                Err(e) => {
                    item.report(
                        "failed",
                        0,
                        0,
                        Some(format!("Steam couldn't subscribe: {e}")),
                    );
                    item.finished = true;
                }
            }
        }

        for item in items.iter_mut().filter(|i| !i.finished) {
            let file = PublishedFileId(item.id);
            let state = ugc.item_state(file);
            let busy =
                ItemState::NEEDS_UPDATE | ItemState::DOWNLOADING | ItemState::DOWNLOAD_PENDING;
            if state.contains(ItemState::SUBSCRIBED | ItemState::INSTALLED)
                && !state.intersects(busy)
            {
                let size = ugc.item_install_info(file).map_or(0, |i| i.size_on_disk);
                item.report("installed", size, size, None);
                item.finished = true;
                last_activity = Instant::now();
                continue;
            }
            let progress = ugc.item_download_info(file).unwrap_or((0, 0));
            if progress != item.last_progress {
                item.last_progress = progress;
                last_activity = Instant::now();
            }
            if state.contains(ItemState::DOWNLOADING) || progress.1 > 0 {
                item.report("downloading", progress.0, progress.1, None);
            } else if state.contains(ItemState::DOWNLOAD_PENDING) {
                item.report("queued", 0, 0, None);
            } else if state.contains(ItemState::SUBSCRIBED) && item.last_nudge.elapsed() > NUDGE {
                ugc.download_item(file, true);
                item.last_nudge = Instant::now();
            }
            if last_activity.elapsed() > STALL {
                item.report(
                    "failed",
                    progress.0,
                    progress.1,
                    Some("Steam stopped downloading".into()),
                );
                item.finished = true;
            }
        }

        if items.iter().all(|i| i.finished) {
            emit(json!({"event": "done"}));
            break;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// Unsubscribes from each item, reporting each as Steam confirms it.
fn unsubscribe(client: &Client, ids: &[u64], stop: &AtomicBool) {
    let ugc = client.ugc();
    let report = |id: u64, state: &str, message: Option<String>| {
        let mut event = json!({"event": "item", "id": id, "state": state});
        if let Some(message) = message {
            event["message"] = json!(message);
        }
        emit(event);
    };
    let (tx, rx) = mpsc::channel::<(u64, Result<(), String>)>();
    let mut waiting: Vec<u64> = Vec::new();
    for &id in ids {
        let file = PublishedFileId(id);
        if !ugc.item_state(file).contains(ItemState::SUBSCRIBED) {
            report(id, "unsubscribed", Some("it wasn't subscribed".into()));
            continue;
        }
        report(id, "unsubscribing", None);
        let tx = tx.clone();
        ugc.unsubscribe_item(file, move |result| {
            let _ = tx.send((id, result.map_err(|e| e.to_string())));
        });
        waiting.push(id);
    }
    let deadline = Instant::now() + UNSUBSCRIBE_TIMEOUT;
    while !waiting.is_empty() {
        if stop.load(Ordering::SeqCst) {
            std::process::exit(3);
        }
        client.run_callbacks();
        while let Ok((id, result)) = rx.try_recv() {
            waiting.retain(|&w| w != id);
            match result {
                Ok(()) => report(id, "unsubscribed", None),
                Err(e) => report(
                    id,
                    "failed",
                    Some(format!("Steam couldn't unsubscribe: {e}")),
                ),
            }
        }
        if Instant::now() > deadline {
            for &id in &waiting {
                report(id, "failed", Some("Steam didn't answer".into()));
            }
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    emit(json!({"event": "done"}));
}
