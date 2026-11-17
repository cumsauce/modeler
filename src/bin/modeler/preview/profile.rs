//! `MODELER_PROFILE=1`: every 2 s, stderr gets the frame time (mean / max,
//! main-thread update share) and the time spent in the preview's heavy
//! systems (those that open a [`span`]), so lag can be traced to a system.

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use bevy::prelude::*;

pub(crate) fn enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var("MODELER_PROFILE").is_ok_and(|v| v != "0" && !v.is_empty()))
}

#[derive(Default)]
struct Stats {
    spans: HashMap<&'static str, (Duration, Duration, u32)>,
    frames: Vec<Duration>,
    update: Vec<Duration>,
    last_frame: Option<Instant>,
    update_start: Option<Instant>,
    window: Option<Instant>,
}

fn stats() -> &'static Mutex<Stats> {
    static S: OnceLock<Mutex<Stats>> = OnceLock::new();
    S.get_or_init(Default::default)
}

/// Times its scope under `name` (a no-op unless profiling).
pub(crate) struct Span(Option<(&'static str, Instant)>);

pub(crate) fn span(name: &'static str) -> Span {
    Span(enabled().then(|| (name, Instant::now())))
}

impl Drop for Span {
    fn drop(&mut self) {
        let Some((name, t)) = self.0 else {
            return;
        };
        let d = t.elapsed();
        if let Ok(mut s) = stats().lock() {
            let e = s.spans.entry(name).or_default();
            e.0 += d;
            e.1 = e.1.max(d);
            e.2 += 1;
        }
    }
}

fn frame_start() {
    let Ok(mut s) = stats().lock() else {
        return;
    };
    let now = Instant::now();
    if let Some(t) = s.last_frame {
        let d = now - t;
        s.frames.push(d);
    }
    s.last_frame = Some(now);
    s.update_start = Some(now);
    let w = *s.window.get_or_insert(now);
    if now - w < Duration::from_secs(2) || s.frames.is_empty() {
        return;
    }
    s.window = Some(now);
    let ms = |d: Duration| d.as_secs_f64() * 1e3;
    let n = s.frames.len() as f64;
    let mean = s.frames.iter().map(|d| ms(*d)).sum::<f64>() / n;
    let max = s.frames.iter().map(|d| ms(*d)).fold(0.0, f64::max);
    let mut sorted: Vec<f64> = s.frames.iter().map(|d| ms(*d)).collect();
    sorted.sort_by(f64::total_cmp);
    let p95 = sorted[((sorted.len() - 1) as f64 * 0.95) as usize];
    let upd = s.update.iter().map(|d| ms(*d)).sum::<f64>() / s.update.len().max(1) as f64;
    let mut line = format!(
        "profile: {} frames, frame mean {mean:.1} ms p95 {p95:.1} max {max:.1}, main-thread update mean {upd:.1} ms",
        s.frames.len()
    );
    let mut spans: Vec<_> = s.spans.drain().collect();
    spans.sort_by_key(|s| std::cmp::Reverse(s.1.0));
    for (name, (total, mx, count)) in spans.iter().take(8) {
        line += &format!(
            "; {name} {:.1} ms/frame (max {:.1}, {count}x)",
            ms(*total) / n,
            ms(*mx)
        );
    }
    eprintln!("{line}");
    s.frames.clear();
    s.update.clear();
}

fn update_end() {
    let Ok(mut s) = stats().lock() else {
        return;
    };
    if let Some(t) = s.update_start.take() {
        s.update.push(t.elapsed());
    }
}

pub(crate) fn profile_plugin(app: &mut App) {
    if enabled() {
        app.add_systems(First, frame_start)
            .add_systems(Last, update_end);
    }
}
