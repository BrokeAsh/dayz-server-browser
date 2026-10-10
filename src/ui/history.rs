//! Keeps the history recorder (see `crate::history`) told which servers to record, and draws the
//! details panel's chart of a server's players over the last day or week.

use eframe::egui::{self, RichText, Sense};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use super::{App, ERROR, Msg};
use crate::a2s::Pong;
use crate::history::{self, Event, KEEP, Recorder, Sample, now_minute};
use crate::settings::data_dir;

const DAY: u32 = 24 * 60;
/// Under this many answers (an hour's worth), the chart says how history is recorded.
const LITTLE: usize = 12;

pub(super) struct HistoryUi {
    recorder: Recorder,
    /// The servers the recorder was last given, and when that was checked.
    targets: Vec<String>,
    checked: Option<Instant>,
    /// The history of the server in the details panel.
    shown: Option<Shown>,
    /// The chart covers the last 7 days rather than 24 hours.
    week: bool,
}

struct Shown {
    addr: String,
    /// Oldest first; `None` until read.
    samples: Option<Vec<Sample>>,
    /// When it usually restarts (local minutes after midnight), if that's clear.
    schedule: Option<Vec<u16>>,
}

impl HistoryUi {
    pub(super) fn new(ctx: &egui::Context, tx: &mpsc::Sender<Msg>) -> Self {
        let (ctx, tx) = (ctx.clone(), tx.clone());
        let recorder = Recorder::spawn(data_dir().join("history"), move |event| {
            let _ = tx.send(Msg::History(event));
            ctx.request_repaint();
        });
        Self {
            recorder,
            targets: Vec::new(),
            checked: None,
            shown: None,
            week: false,
        }
    }
}

impl Shown {
    fn samples_changed(&mut self) {
        let tz = jiff::tz::TimeZone::system();
        self.schedule = self.samples.as_deref().and_then(|samples| {
            history::restart_schedule(samples, |minute| {
                jiff::Timestamp::from_second(i64::from(minute) * 60)
                    .map_or(0, |t| tz.to_offset(t).seconds() / 60)
            })
        });
    }
}

impl App {
    /// The servers whose players are recorded: favorites, then recent and watched ones.
    pub(super) fn history_targets(&self) -> Vec<String> {
        let mut targets: Vec<String> = Vec::new();
        let recent = self.state.recent.iter().map(|r| &r.addr);
        let watched = self.state.watched.iter().map(|w| &w.addr);
        for addr in self.state.favorites.iter().chain(recent).chain(watched) {
            if !targets.contains(addr) {
                targets.push(addr.clone());
            }
        }
        targets
    }

    /// Tells the recorder when the servers to record change (checked once a second).
    pub(super) fn history_now_and_then(&mut self) {
        if self
            .history
            .checked
            .is_some_and(|t| t.elapsed() < Duration::from_secs(1))
        {
            return;
        }
        self.history.checked = Some(Instant::now());
        let targets = self.history_targets();
        if targets != self.history.targets {
            self.history.recorder.set_targets(targets.clone());
            self.history.targets = targets;
        }
    }

    /// Records a ping's answer from a server whose history is kept.
    pub(super) fn history_answer(&self, pong: &Pong) {
        if let Some(info) = &pong.info
            && self.history.targets.contains(&pong.addr)
        {
            self.history.recorder.record_answer(pong.addr.clone(), info);
        }
    }

    pub(super) fn history_event(&mut self, event: Event) {
        let Some(shown) = &mut self.history.shown else {
            return;
        };
        match event {
            Event::Loaded(addr, samples) if addr == shown.addr => {
                shown.samples = Some(samples);
                shown.samples_changed();
            }
            Event::Recorded(addr, sample) if addr == shown.addr => {
                let Some(samples) = &mut shown.samples else {
                    return; // Still being read; that includes this one.
                };
                let at = samples.partition_point(|s| s.minute <= sample.minute);
                samples.insert(at, sample);
                let cutoff = sample.minute.saturating_sub(KEEP);
                samples.retain(|s| s.minute >= cutoff);
                shown.samples_changed();
            }
            _ => {}
        }
    }

    /// The details panel's players chart, with when the server usually restarts. Shown for a
    /// server with recorded history, or one being recorded (with a word on how).
    pub(super) fn history_section(&mut self, ui: &mut egui::Ui, addr: &str) {
        if self.history.shown.as_ref().is_none_or(|s| s.addr != addr) {
            self.history.shown = Some(Shown {
                addr: addr.to_string(),
                samples: None,
                schedule: None,
            });
            self.history.recorder.load(addr.to_string());
        }
        let recording = self.history.targets.iter().any(|t| t == addr);
        let week = &mut self.history.week;
        let Some(Shown {
            samples: Some(samples),
            schedule,
            ..
        }) = &self.history.shown
        else {
            return;
        };
        let answers = samples.iter().filter(|s| s.answered).count();
        if answers == 0 && !recording {
            ui.add_space(8.0);
            ui.label(
                RichText::new("Favorite or watch this server to record its player history.")
                    .weak()
                    .small(),
            );
            return;
        }
        let now = now_minute();
        let from = now.saturating_sub(if *week { 7 * DAY } else { DAY });
        let window = &samples[samples.partition_point(|s| s.minute < from)..];

        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new("Player history").strong());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.selectable_label(*week, "7 days").clicked() {
                    *week = true;
                }
                if ui.selectable_label(!*week, "24 h").clicked() {
                    *week = false;
                }
            });
        });
        if window.iter().any(|s| s.answered) {
            chart(ui, window, from, now, *week);
        } else if answers > 0 {
            let span = if *week { "7 days" } else { "24 hours" };
            ui.label(
                RichText::new(format!("Nothing recorded in the last {span}."))
                    .weak()
                    .small(),
            );
        }
        if recording && answers < LITTLE {
            ui.label(
                RichText::new("History is recorded while the browser is open.")
                    .weak()
                    .small(),
            );
        }
        if let Some(times) = schedule {
            let times: Vec<String> = times
                .iter()
                .map(|t| format!("{:02}:{:02}", t / 60, t % 60))
                .collect();
            ui.label(
                RichText::new(format!("Usually restarts around {}", times.join(", "))).small(),
            )
            .on_hover_text(
                "Your local time, going by when the server stopped answering or its in-game \
                 clock was set back while the browser was open.",
            );
        }
    }
}

/// Players over `from..to` (Unix minutes): a line through the answers, broken where nothing was
/// recorded, and a red tick where the server didn't answer. Hovering shows the time and count.
fn chart(ui: &mut egui::Ui, samples: &[Sample], from: u32, to: u32, week: bool) {
    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 76.0), Sense::hover());
    let visuals = ui.visuals();
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 3.0, visuals.extreme_bg_color);
    let plot = egui::Rect::from_min_max(
        rect.min + egui::vec2(4.0, 4.0),
        rect.max - egui::vec2(4.0, 16.0),
    );
    let span = to.saturating_sub(from).max(1);
    let top = samples
        .iter()
        .filter(|s| s.answered)
        .map(|s| s.slots.max(s.players))
        .max()
        .unwrap_or(0)
        .max(1);
    let x = |minute: u32| {
        plot.left() + minute.saturating_sub(from).min(span) as f32 / span as f32 * plot.width()
    };
    let y = |players: u16| plot.bottom() - f32::from(players) / f32::from(top) * plot.height();

    let weak = visuals.weak_text_color();
    let grid = egui::Stroke::new(1.0, visuals.widgets.noninteractive.bg_stroke.color);
    let small = egui::FontId::proportional(10.0);
    painter.hline(plot.x_range(), y(top), grid);
    painter.hline(plot.x_range(), plot.bottom(), grid);
    painter.text(
        egui::pos2(plot.left() + 1.0, y(top) + 1.0),
        egui::Align2::LEFT_TOP,
        top.to_string(),
        small.clone(),
        weak,
    );
    painter.text(
        egui::pos2(plot.left(), rect.bottom() - 2.0),
        egui::Align2::LEFT_BOTTOM,
        if week { "7 days ago" } else { "24 hours ago" },
        small.clone(),
        weak,
    );
    painter.text(
        egui::pos2(plot.right(), rect.bottom() - 2.0),
        egui::Align2::RIGHT_BOTTOM,
        "now",
        small,
        weak,
    );

    // Answers further apart than this (minutes) weren't recorded in between.
    let gap = if week { 30 } else { 12 };
    let line = egui::Stroke::new(1.5, visuals.hyperlink_color);
    let mut run: Vec<egui::Pos2> = Vec::new();
    let flush = |run: &mut Vec<egui::Pos2>| {
        match run.len() {
            0 => {}
            1 => {
                painter.circle_filled(run[0], 1.5, line.color);
            }
            _ => {
                painter.add(egui::Shape::line(std::mem::take(run), line));
            }
        }
        run.clear();
    };
    let mut last: Option<u32> = None;
    for s in samples {
        if !s.answered {
            flush(&mut run);
            last = None;
            painter.vline(
                x(s.minute),
                plot.bottom() - 4.0..=plot.bottom(),
                egui::Stroke::new(1.5, ERROR),
            );
            continue;
        }
        if last.is_some_and(|m| s.minute.saturating_sub(m) > gap) {
            flush(&mut run);
        }
        run.push(egui::pos2(x(s.minute), y(s.players)));
        last = Some(s.minute);
    }
    flush(&mut run);

    let Some(pointer) = response.hover_pos() else {
        return;
    };
    // Minutes into the chart (Unix minutes are too big for an `f32`).
    let at = ((pointer.x - plot.left()) / plot.width()).clamp(0.0, 1.0) * span as f32;
    let off = |s: &Sample| (s.minute.saturating_sub(from) as f32 - at).abs();
    // The nearest sample, if it's within a few pixels.
    let reach = (6.0 / plot.width() * span as f32).max(3.0);
    let Some(s) = samples
        .iter()
        .min_by(|a, b| off(a).total_cmp(&off(b)))
        .filter(|s| off(s) <= reach)
    else {
        return;
    };
    painter.vline(x(s.minute), plot.y_range(), egui::Stroke::new(1.0, weak));
    let text = if s.answered {
        painter.circle_filled(egui::pos2(x(s.minute), y(s.players)), 3.0, line.color);
        format!(
            "{}: {} of {} players",
            local_time(s.minute, week),
            s.players,
            s.slots
        )
    } else {
        format!("{}: didn't answer", local_time(s.minute, week))
    };
    response.on_hover_text_at_pointer(text);
}

/// A Unix minute as local time, `14:05`, or `Tue 14:05` over a week.
fn local_time(minute: u32, week: bool) -> String {
    let Ok(t) = jiff::Timestamp::from_second(i64::from(minute) * 60) else {
        return String::new();
    };
    let local = t.to_zoned(jiff::tz::TimeZone::system());
    local
        .strftime(if week { "%a %H:%M" } else { "%H:%M" })
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Draws the chart off screen, with the pointer over it, through gaps, silences, and a server
    /// with no slots.
    #[test]
    fn draws_chart() {
        let to = 29_000_000;
        let from = to - DAY;
        let mut samples: Vec<Sample> = (from..to)
            .step_by(5)
            .map(|minute| Sample {
                minute,
                players: (minute % 70) as u16,
                slots: 60,
                game_time: None,
                answered: true,
            })
            .collect();
        samples.retain(|s| !(from + 300..from + 400).contains(&s.minute));
        samples[10] = Sample::silent(samples[10].minute);
        let empty_server = [Sample {
            slots: 0,
            players: 0,
            ..samples[0]
        }];
        let ctx = egui::Context::default();
        for (samples, week) in [
            (&samples[..], false),
            (&samples[..], true),
            (&empty_server[..], false),
        ] {
            for x in [10.0, 150.0, 290.0] {
                for _ in 0..3 {
                    let input = egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(
                            egui::Pos2::ZERO,
                            egui::vec2(300.0, 200.0),
                        )),
                        events: vec![egui::Event::PointerMoved(egui::pos2(x, 30.0))],
                        ..Default::default()
                    };
                    let mut output = ctx.run_ui(input, |ui| chart(ui, samples, from, to, week));
                    assert!(!output.shapes.is_empty());
                    // Fonts would go to the GPU here.
                    output.textures_delta.clear();
                }
            }
        }
    }
}
